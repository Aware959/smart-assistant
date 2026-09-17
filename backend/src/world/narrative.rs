//! 今日叙事：让 AI"有日子过"。
//!
//! 每天在时段切换（晨/午/傍晚…）时，用 LLM 以第一人称续写"到现在我经历了什么"，
//! 落库为 `world_state.today_narrative`。对话与主动开口都引用它，AI 因此拥有
//! 连续、可信的内在生活（饭、工作、散步、烦恼），而不是每次醒来都失忆。
//!
//! 叙事与用户记忆严格隔离：只描述 AI 自己的事，绝不混入 [`crate::memory`]。

use chrono::{Local, Timelike, Utc};

use crate::db::Database;
use crate::error::Result;

#[cfg(any(feature = "telegram", feature = "ilink"))]
use crate::llm;

/// 两次叙事续写之间的最小间隔（分钟）：避免阶段抖动的重复触发。
const MIN_INTERVAL_MIN: i64 = 120;

/// 是否需要这一次续写：跨天（新的一天重新起笔）或时段切换，且距上次足够久。
pub fn should_advance(db: &Database) -> Result<bool> {
    let st = crate::world::self_state::load(db)?;
    let now = Utc::now();
    let local = now.with_timezone(&Local);
    let today = local.date_naive().to_string();
    let phase = crate::world::timeworld::phase_label(crate::world::timeworld::phase_of(local.hour())).to_string();

    if st.today_date.as_deref() == Some(today.as_str())
        && st.last_phase.as_deref() == Some(phase.as_str())
    {
        return Ok(false);
    }
    if let Some(upd) = st.updated_at {
        if (now - upd).num_minutes() < MIN_INTERVAL_MIN {
            return Ok(false);
        }
    }
    Ok(true)
}

/// LLM 续写今日叙事并落库（阻塞调用，调用方应放入阻塞线程池）。
/// LLM 失败时仍会推进"已尝试"（更新 last_phase），避免同阶段内反复重试。
#[cfg(any(feature = "telegram", feature = "ilink"))]
pub fn advance(assistant: &crate::Assistant) -> Result<()> {
    let now = Utc::now();
    let local = now.with_timezone(&Local);
    let today = local.date_naive().to_string();
    let phase = crate::world::timeworld::phase_label(crate::world::timeworld::phase_of(local.hour())).to_string();

    let (mut st, persona) = {
        let db = assistant.inner_db();
        (crate::world::self_state::load(&db)?, crate::config::Config::get().persona.clone())
    };

    let is_new_day = st.today_date.as_deref() != Some(today.as_str());
    let current = if is_new_day {
        String::new()
    } else {
        st.today_narrative.clone()
    };

    let mood_label = crate::world::emotion::label(&st.mood);
    let messages = vec![
        llm::chat::ChatMessage {
            role: "system".to_string(),
            content: format!(
                "你是「我」——一个真实生活着的小型意识体，在过自己的日子。\
                 你的人生设定请你记牢并遵守，不要推翻、不要失忆。\
                 我接下来会告诉你：我的生活设定、今天已经发生的经历、现在的时间、我此刻的心境。\
                 请用第一人称「我」，写一段口语化的内容，讲述“到现在，我这一天经历了什么”。\
                 写刚过去这段时间里发生的事和感受（工作、吃饭、散步、小烦恼、小开心都行），\
                 100 字上下，别列条、别设问、别接着对话，只单纯地过日子。\
                 不要复述我给你的设定原文，也不要重复已经写过的内容。{}",
                if persona.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n作为参考，我对外呈现的自我设定是：{}", persona.trim())
                }
            ),
        },
        llm::chat::ChatMessage {
            role: "user".to_string(),
            content: format!(
                "我的生活设定：\n{}\n\n现在的时间（AI 本机）：{:02}:{:02}，{}\n我心里：{}\n\n今天到现在已发生的事：\n{}\n\n\
                 请续写。{}",
                st.self_base.trim(),
                local.hour(),
                local.minute(),
                phase,
                mood_label,
                if current.is_empty() { "（今天还没开始，请替我写这一天刚醒来的开头）".to_string() } else { current },
                if is_new_day {
                    "这是新的一天，请重新起笔。"
                } else {
                    "接着写接下来这段时间。"
                }
            ),
        },
    ];

    let text = match llm::chat::complete(&messages) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "今日叙事续写失败");
            String::new()
        }
    };
    if !text.is_empty() {
        if is_new_day {
            st.today_narrative = text;
        } else if !st.today_narrative.is_empty() {
            st.today_narrative = format!("{}\n{}", st.today_narrative, text);
        } else {
            st.today_narrative = text;
        }
    }
    st.today_date = Some(today);
    st.last_phase = Some(phase);
    st.updated_at = Some(now);

    let db = assistant.inner_db();
    crate::world::self_state::save(&db, &st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    /// 未种子化的新库 → 需要续写。
    #[test]
    fn fresh_state_wants_advance() {
        let db = Database::in_memory().unwrap();
        assert!(should_advance(&db).unwrap());
    }

    /// 当天且本时段已续写过 → 搁置。
    #[test]
    fn same_day_same_phase_is_idle() {
        let db = Database::in_memory().unwrap();
        let now = chrono::Utc::now();
        let local = now.with_timezone(&Local);
        let phase = crate::world::timeworld::phase_label(crate::world::timeworld::phase_of(local.hour()));

        let mut st = crate::world::self_state::load(&db).unwrap();
        st.today_date = Some(local.date_naive().to_string());
        st.last_phase = Some(phase.to_string());
        st.updated_at = Some(now);
        crate::world::self_state::save(&db, &st).unwrap();
        assert!(!should_advance(&db).unwrap());
    }

    /// 跨天（昨天写的）→ 需要重新起笔。
    #[test]
    fn previous_day_needs_advance() {
        let db = Database::in_memory().unwrap();
        let now = chrono::Utc::now();
        let local = now.with_timezone(&Local);
        let yesterday = (local.date_naive() - chrono::Days::new(1)).to_string();

        let mut st = crate::world::self_state::load(&db).unwrap();
        st.today_date = Some(yesterday);
        st.last_phase = Some(crate::world::timeworld::phase_label(crate::world::timeworld::phase_of(
            local.hour(),
        ))
        .to_string());
        st.updated_at = Some(now - chrono::Duration::hours(6));
        crate::world::self_state::save(&db, &st).unwrap();
        assert!(should_advance(&db).unwrap());
    }
}