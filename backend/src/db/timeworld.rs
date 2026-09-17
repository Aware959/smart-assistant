//! 时间世界模型的持久化：`time_profiles` 表（推断出的对方作息画像）的读写，
//! 以及推断画像所需的用户历史消息时刻读取。
//!
//! 时区 / 活跃小时的推断算法在 [`crate::world::timeworld`]，本模块只负责存取原始数据。

use chrono::{DateTime, Timelike, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;

/// 一条作息画像（均为通道 + 外部用户维度）。
#[derive(Debug, Clone)]
pub struct TimeProfile {
    /// 推断的时区偏移（分钟，东为正）。
    pub utc_offset_minutes: i32,
    /// 对方历史最活跃小时（UTC）。
    pub active_hour: u32,
    /// 已观测到的用户消息条数。
    pub observations: i64,
}

/// 写（upsert）一条作息画像。
pub fn upsert_profile(
    db: &Database,
    channel: &str,
    external_id: &str,
    offset_minutes: i32,
    active_hour: u32,
    observations: i64,
) -> Result<()> {
    db.conn().execute(
        "INSERT INTO time_profiles (channel, external_id, utc_offset_minutes, active_hour, observations, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(channel, external_id) DO UPDATE SET
           utc_offset_minutes = excluded.utc_offset_minutes,
           active_hour       = excluded.active_hour,
           observations      = excluded.observations,
           updated_at        = excluded.updated_at",
        params![
            channel,
            external_id,
            offset_minutes,
            active_hour as i64,
            observations,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// 读作息画像（无画像或观测数为 0 时返回 None）。
pub fn profile(db: &Database, channel: &str, external_id: &str) -> Result<Option<TimeProfile>> {
    let mut stmt = db.conn().prepare(
        "SELECT utc_offset_minutes, active_hour, observations
         FROM time_profiles WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |r| {
        Ok((
            r.get::<_, Option<i32>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    match rows.next() {
        Some(Ok((off, ah, n))) if n > 0 => Ok(off.map(|o| TimeProfile {
            utc_offset_minutes: o,
            active_hour: ah.unwrap_or(0) as u32,
            observations: n,
        })),
        Some(Ok(_)) => Ok(None),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// 读某用户全部入站消息的 UTC 小时（画像推断的原始依据）。无消息返回空列表。
pub fn user_message_hours(db: &Database, channel: &str, external_id: &str) -> Result<Vec<u32>> {
    let mut stmt = db.conn().prepare(
        "SELECT m.created_at
         FROM messages m
         JOIN channel_sessions cs ON cs.session_id = m.session_id
         WHERE cs.channel = ?1 AND cs.external_id = ?2 AND m.role = 'user'",
    )?;
    let hours = stmt
        .query_map(params![channel, external_id], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|ts| DateTime::parse_from_rfc3339(&ts).ok())
        .map(|dt| dt.with_timezone(&Utc).hour())
        .collect::<Vec<u32>>();
    Ok(hours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn add_user(db: &Database, channel: &str, external: &str) -> String {
        crate::db::channel::get_or_create_session(db, channel, external)
            .unwrap()
            .id
    }

    #[test]
    fn upsert_and_read_profile() {
        let db = Database::in_memory().unwrap();
        assert!(profile(&db, "ilink", "o@im.wechat").unwrap().is_none());

        upsert_profile(&db, "ilink", "o@im.wechat", 480, 3, 4).unwrap();
        let p = profile(&db, "ilink", "o@im.wechat").unwrap().unwrap();
        assert_eq!(p.utc_offset_minutes, 480);
        assert_eq!(p.active_hour, 3);
        assert_eq!(p.observations, 4);

        // 再次写入为 upsert，仍是一行。
        upsert_profile(&db, "ilink", "o@im.wechat", 480, 4, 5).unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM time_profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(profile(&db, "ilink", "o@im.wechat").unwrap().unwrap().active_hour, 4);
    }

    #[test]
    fn reads_user_message_hours_across_sessions() {
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "ilink", "o@im.wechat");
        let at = DateTime::parse_from_rfc3339("2026-09-14T03:47:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        crate::db::message::create_at(&db, &sid, "user", "x", at).unwrap();
        crate::db::message::create_at(&db, &sid, "assistant", "y", at).unwrap();

        let hours = user_message_hours(&db, "ilink", "o@im.wechat").unwrap();
        assert_eq!(hours, vec![3], "只统计 user 消息的 UTC 小时");
    }
}