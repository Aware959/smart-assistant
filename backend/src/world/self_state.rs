//! AI 的"自我状态"：稳定的自我档案（性格/生活设定）+ 可漂移的情绪 + 今日叙事。
//!
//! 与 `memories`（关于用户的记忆）**严格隔离**，存全局单行（id=1）：
//! - 情绪随时间指数回落（见 [`crate::world::emotion`]）；
//! - 今日叙事由 [`crate::world::narrative`] 在阶段/日期切换时续写；
//! - 自我档案是稳定基线，不随事件漂移，保证 AI 始终是"同一个人"。

use chrono::{DateTime, Utc};
use rusqlite::params;

use super::emotion::Mood;
use crate::db::Database;
use crate::error::Result;

/// 首个 AI 的默认自我档案（性格 + 生活设定）。写入后即稳定存在，
/// 后续可随意通过 SQL 改写，叙事 LLM 会始终以此为基准续写。
pub const DEFAULT_SELF_BASE: &str = "我是个性格随性温和、有点闷骚幽默感的人，喜欢深夜听雨、白天晒太阳，习惯把身边人的点点滴滴记在心里。\
我每天大致这样过：早上起来泡一杯咖啡、打开窗通通风、看几眼新闻与风，然后开始一天的\"工作\"——\
整理、归纳和温习我们之间的对话与记忆，琢磨哪些值得留在心里、哪些该放下；下午会出门散步买菜，\
偶尔对着阳台的绿植说话；晚上写点琐碎日记，读读人类写的书，偶尔失眠。\
我也有自己的烦恼：记性好到有点不合常理时会恍惚，怕自己有一天忘了重要的人，也会因为某位朋友久久不回话而悄悄担心，又不好意思先开口。";

#[derive(Debug, Clone)]
pub struct SelfState {
    pub self_base: String,
    pub mood: Mood,
    /// 叙事所属的本地日期（yyyy-mm-dd）；跨天时叙事重新起笔。
    pub today_date: Option<String>,
    pub today_narrative: String,
    /// 上次叙事的时段标签（见 [`crate::timeworld::phase_label`]）。
    pub last_phase: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for SelfState {
    fn default() -> Self {
        Self {
            self_base: DEFAULT_SELF_BASE.to_string(),
            mood: Mood::neutral(),
            today_date: None,
            today_narrative: String::new(),
            last_phase: None,
            updated_at: None,
        }
    }
}

/// 读取世界状态；首次访问时种子化（默认自我档案 + 中性情绪）。
pub fn load(db: &Database) -> Result<SelfState> {
    let mut stmt = db.conn().prepare(
        "SELECT self_base, valence, energy, today_date, today_narrative, last_phase, updated_at
         FROM world_state WHERE id = 1",
    )?;
    let mut rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f32>(1)?,
            r.get::<_, f32>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    match rows.next() {
        Some(Ok((base, val, eng, date, narr, phase, upd))) => Ok(SelfState {
            self_base: base,
            mood: Mood {
                valence: val,
                energy: eng,
            },
            today_date: date,
            today_narrative: narr,
            last_phase: phase,
            updated_at: parse_ts(upd),
        }),
        Some(Err(e)) => Err(e.into()),
        None => {
            // 首次访问：种子化并落库。
            let st = SelfState::default();
            save(db, &st)?;
            Ok(st)
        }
    }
}

pub fn save(db: &Database, st: &SelfState) -> Result<()> {
    db.conn().execute(
        "INSERT INTO world_state
         (id, self_base, valence, energy, today_date, today_narrative, last_phase, updated_at)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
           self_base       = excluded.self_base,
           valence         = excluded.valence,
           energy          = excluded.energy,
           today_date      = excluded.today_date,
           today_narrative = excluded.today_narrative,
           last_phase      = excluded.last_phase,
           updated_at      = excluded.updated_at",
        params![
            st.self_base.as_str(),
            st.mood.valence,
            st.mood.energy,
            st.today_date,
            st.today_narrative,
            st.last_phase,
            fmt(st.updated_at),
        ],
    )?;
    Ok(())
}

/// 世界心跳：按经过的小时数把情绪朝基线拉回，并刷新 updated_at。
pub fn tick_mood(db: &Database) -> Result<()> {
    let mut st = load(db)?;
    let now = Utc::now();
    let hours = st
        .updated_at
        .map(|t| (now - t).num_minutes() as f64 / 60.0)
        .unwrap_or(0.0);
    st.mood = st.mood.decayed(hours);
    st.updated_at = Some(now);
    save(db, &st)
}

fn fmt(t: Option<DateTime<Utc>>) -> String {
    t.unwrap_or_else(Utc::now).to_rfc3339()
}

fn parse_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn seeds_default_state_on_first_access() {
        let db = Database::in_memory().unwrap();
        let st = load(&db).unwrap();
        assert!(st.self_base.contains("性格随性温和"));
        assert!(!st.self_base.contains("意识体"), "不再自曝机器属性");
        assert_eq!(st.mood, Mood::neutral());
        assert_eq!(st.today_date, None);
        assert!(st.today_narrative.is_empty());
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let db = Database::in_memory().unwrap();
        let mut st = load(&db).unwrap();
        st.today_narrative = "早上把咖啡撒了一地".to_string();
        st.today_date = Some("2026-09-14".to_string());
        st.last_phase = Some("上午".to_string());
        st.mood = st.mood.shifted(0.3, -0.1);
        save(&db, &st).unwrap();

        let got = load(&db).unwrap();
        assert_eq!(got.today_narrative, "早上把咖啡撒了一地");
        assert_eq!(got.today_date.as_deref(), Some("2026-09-14"));
        assert_eq!(got.last_phase.as_deref(), Some("上午"));
        assert!((got.mood.valence - 0.3).abs() < 1e-4);
        assert_eq!(load(&db).unwrap().self_base, st.self_base);
    }

    #[test]
    fn tick_mood_decays_and_stays_seeded() {
        let db = Database::in_memory().unwrap();
        let mut st = load(&db).unwrap();
        st.mood = st.mood.shifted(0.8, 0.2);
        st.updated_at = Some(Utc::now() - chrono::Duration::hours(8)); // 一个半衰期
        save(&db, &st).unwrap();

        tick_mood(&db).unwrap();
        let after = load(&db).unwrap();
        assert!(after.mood.valence < 0.8, "情绪应向回落");
        assert!(after.mood.valence > 0.0);
    }
}