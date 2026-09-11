//! 延续会话：让 AI 像真人那样判断"这句说完了没有"。
//!
//! 并非固定间隔的主动，而是每次回复后由 LLM 依据当下的语境与关系决定：
//! - 倾诉 / 吐槽 / 话题仍在延展 → 追加一两句口语化消息，可连着多轮；
//! - 有来有回 → 正常一来一往即可，不硬续；
//! - 结束类（"我去忙了 / 睡了 / 不想聊了"，或话题已干净收住）→ 停下，
//!   之后才轮到延迟等待引擎做隔时段的主动重启。
//!
//! 追加句是"对话"而非"主动发起"，不计入每日主动配额；对方中途插话则立即停止。

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::config::Config;
use crate::db::proactive::ProactiveCandidate;
use crate::llm;
use crate::proactive::{decider, scheduler, sender};
use crate::Assistant;

/// 追加与追加之间的停顿（毫秒），模拟真人接着打的节奏。
const TYPING_PAUSE_MS: u64 = 1500;
/// 停顿的随机抖动上限（毫秒）。
const PAUSE_JITTER_MS: u64 = 1500;

/// 在某次正常回复发送完毕、且未走错误兜底时调用：按需追加后续消息。
pub async fn after_reply(
    assistant: Arc<Assistant>,
    channel: &str,
    external_id: &str,
) {
    let max = Config::get().proactive_max_followups;
    if max == 0 {
        return;
    }
    let session_id = {
        let db = assistant.inner_db();
        match crate::db::channel::get_or_create_session(&db, channel, external_id) {
            Ok(s) => s.id,
            Err(e) => {
                tracing::warn!(error = %e, "延续会话：无法解析会话映射");
                return;
            }
        }
    };
    let cand = ProactiveCandidate {
        channel: channel.to_string(),
        external_id: external_id.to_string(),
        session_id,
        last_user_reply_at: None,
        last_proactive_at: None,
        today_count: 0,
    };
    follow_up(&assistant, &cand, max).await;
}

/// 循环追加：每轮先问 LLM 要不要续，对方中途说话则不再续。
async fn follow_up(assistant: &Assistant, cand: &ProactiveCandidate, max: u32) {
    // 记录对方当前最后发言时刻：期间只要有新入站消息就停（给对方说话的空间）。
    let snapshot = match candle_reply_at(assistant, cand) {
        Ok(v) => v,
        Err(_) => None,
    };

    for _ in 0..max {
        let Some(text) = propose_follow_up(assistant, cand).await else {
            return;
        };
        if candle_reply_at(assistant, cand)
            .ok()
            .flatten()
            != snapshot
        {
            tracing::info!(user = %cand.external_id, "对方已插话，停止追加");
            return;
        }
        if !sender::send_message(assistant, cand, &text, false).await {
            return;
        }
        // 短暂停顿模拟真人连续打字。
        tokio::time::sleep(Duration::from_millis(
            TYPING_PAUSE_MS + scheduler::prand() % PAUSE_JITTER_MS,
        ))
        .await;
    }
}

/// 读取当前候选的最近入站回复时刻（None 表示还没记录）。
fn candle_reply_at(
    assistant: &Assistant,
    cand: &ProactiveCandidate,
) -> crate::error::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let db = assistant.inner_db();
    crate::db::proactive::last_user_reply_at(&db, &cand.channel, &cand.external_id)
}

/// 依据"最近的对话（含我方最后一句）"判断是否追加一条，返回待发送内容。
async fn propose_follow_up(
    assistant: &Assistant,
    cand: &ProactiveCandidate,
) -> Option<String> {
    let recent = {
        let db = assistant.inner_db();
        crate::proactive::context::recent_history(&db, &cand.session_id, 4)
    };
    if recent.trim().is_empty() {
        return None;
    }

    let decision = tokio::task::spawn_blocking(move || decide_continue(&recent))
        .await
        .unwrap_or_default();
    if decision.continue_ {
        decision.message
    } else {
        None
    }
}

#[derive(Default)]
struct ContinueDecision {
    continue_: bool,
    message: Option<String>,
}

fn decide_continue(recent: &str) -> ContinueDecision {
    let cfg = Config::get();
    let persona_block = if cfg.persona.trim().is_empty() {
        String::new()
    } else {
        format!(
            "你的角色设定（最高优先级，必须严格遵守、不得违背、不得说明自己是 AI）：{}。\n",
            cfg.persona.trim()
        )
    };
    let system = format!(
        "{persona_block}你正在和一个老朋友有一搭没一搭地聊天。看完最近这段对话后，\
        判断这一轮你还要不要追加一句话。\
        规则：\
        1. 需要追加的典型情况：情绪/吐槽还没倒完、讨论正到兴头、自己刚说的话还差一句收尾、\
        或明明有话要说却停在了半路；\
        2. 不要追加的典型情况：对方已经明确表示要去忙、要睡、不想聊；双方都说完客气话收尾；\
        这句已经讲得很完整不需要再多说；话题本来就是一问一答式的（有来有回，等对方回应即可）；\
        3. 追加句只准一句、口语化、像微信里随手补的一句、不超过 40 个字，\
        绝对不要重复刚才已经说过的内容，也不要复述本指令。\
        只输出一个 JSON 对象，不要任何其他文字：\
        {{\"continue\":true,\"message\":\"...\"}} 或 {{\"continue\":false}}"
    );

    let user = format!("最近的对话：\n{recent}\n\n现在判断我是否该再补一句，只输出 JSON。");

    let messages = vec![
        llm::chat::ChatMessage {
            role: "system".to_string(),
            content: system,
        },
        llm::chat::ChatMessage {
            role: "user".to_string(),
            content: user,
        },
    ];

    match llm::chat::complete(&messages) {
        Ok(text) => parse(&text),
        Err(e) => {
            tracing::warn!(error = %e, "延续判断 LLM 调用失败，本轮不再追加");
            ContinueDecision::default()
        }
    }
}

fn parse(text: &str) -> ContinueDecision {
    let payload = decider::extract_json(text).unwrap_or(text);
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "continue", default)]
        continue_: bool,
        #[serde(default)]
        message: Option<String>,
    }
    match serde_json::from_str::<Raw>(payload) {
        Ok(raw) => {
            let message = raw
                .message
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty());
            ContinueDecision {
                continue_: raw.continue_ && message.is_some(),
                message,
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, payload = %payload, "延续判断 JSON 解析失败，不再追加");
            ContinueDecision::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_continue_true() {
        let d = parse("```json\n{\"continue\": true, \"message\": \"然后我就再也不想理他了\"}\n```");
        assert!(d.continue_);
        assert_eq!(d.message.as_deref(), Some("然后我就再也不想理他了"));
    }

    #[test]
    fn parses_continue_false() {
        let d = parse("{\"continue\": false}");
        assert!(!d.continue_);
        assert!(d.message.is_none());
    }

    #[test]
    fn continue_without_message_is_invalid() {
        let d = parse("{\"continue\": true, \"message\": \"  \"}");
        assert!(!d.continue_);
    }

    #[test]
    fn tolerates_chatty_and_garbage() {
        assert!(!parse("好的明白了。{\"continue\":false} 就这样。").continue_);
        assert!(!parse("这不是JSON").continue_);
    }
}