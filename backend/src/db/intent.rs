//! 意图开口状态表（`intent_state` 表）：只有意图领域适配器
//! （[`crate::core::intent::IntentTracker`]）读写。
//! 本模块只做 SQL：返回行数据，不做任何业务过滤 / 映射。

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;

/// 一行候选数据：有会话映射的外部用户 + 其意图状态（未建状态的行以默认值出现）。
/// 纯行数据；领域形状（[`crate::core::intent::IntentCandidate`]）由意图状态追踪器映射。
#[derive(Debug, Clone)]
pub struct IntentRow {
    pub channel: String,
    pub external_id: String,
    pub session_id: String,
    /// 用户最后一次入站消息时间（RFC3339）；从没记录过则为 None。
    pub last_user_reply_at: Option<DateTime<Utc>>,
    /// 上次意图开口（主动发送）时间；从未主动发过则为 None。
    pub last_intent_at: Option<DateTime<Utc>>,
    /// 今日已主动发送条数。
    pub today_count: i64,
}

/// 保证 (channel, external_id) 在状态表有行（read 到当前值）。
pub fn ensure_state(db: &Database, channel: &str, external_id: &str, session_id: &str) -> Result<()> {
    db.conn().execute(
        "INSERT OR IGNORE INTO intent_state
             (channel, external_id, session_id)
         VALUES (?1, ?2, ?3)",
        params![channel, external_id, session_id],
    )?;
    Ok(())
}

/// 用户发来消息时调用：记录"对方刚说话"，并刷新 session 指向。
pub fn touch_user_reply(
    db: &Database,
    channel: &str,
    external_id: &str,
    session_id: &str,
) -> Result<()> {
    ensure_state(db, channel, external_id, session_id)?;
    let now = Utc::now().to_rfc3339();
    db.conn().execute(
        "UPDATE intent_state
         SET last_user_reply_at = ?1, session_id = ?2
         WHERE channel = ?3 AND external_id = ?4",
        params![now, session_id, channel, external_id],
    )?;
    Ok(())
}

/// 意图开口（主动发送）成功后调用：更新最近主动时间与今日计数（跨天自动清零）。
pub fn record_intent_send(
    db: &Database,
    channel: &str,
    external_id: &str,
    session_id: &str,
) -> Result<()> {
    ensure_state(db, channel, external_id, session_id)?;
    let now = Utc::now();
    let today = now.date_naive().to_string();
    let now_rfc = now.to_rfc3339();

    db.conn().execute(
        "UPDATE intent_state
         SET last_intent_at = ?1,
             today_count = CASE WHEN today_date = ?2 THEN today_count + 1 ELSE 1 END,
             today_date = ?3
         WHERE channel = ?4 AND external_id = ?5",
        params![now_rfc, today, today, channel, external_id],
    )?;
    Ok(())
}

/// 读取某用户最近一次入站回复时间（从未记录则为 None）。
pub fn last_user_reply_at(
    db: &Database,
    channel: &str,
    external_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT last_user_reply_at FROM intent_state
         WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |row| {
        row.get::<_, Option<String>>(0)
    })?;
    match rows.next() {
        Some(Ok(v)) => Ok(parse_ts(v)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

fn parse_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// 读取某用户最近一次主动开口（主动外发）时间（从未主动发过则为 None）。
pub fn last_intent_at(
    db: &Database,
    channel: &str,
    external_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT last_intent_at FROM intent_state
         WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |row| {
        row.get::<_, Option<String>>(0)
    })?;
    match rows.next() {
        Some(Ok(v)) => Ok(parse_ts(v)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// 列出全部候选行（有会话映射即候选），未建状态的行以默认统计出现。
pub fn list_rows(db: &Database) -> Result<Vec<IntentRow>> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT
                cs.channel,
                cs.external_id,
                cs.session_id,
                ps.last_user_reply_at,
                ps.last_intent_at,
                COALESCE(ps.today_count, 0)
             FROM channel_sessions cs
             LEFT JOIN intent_state ps
               ON ps.channel = cs.channel AND ps.external_id = cs.external_id",
        )?;
    let rows = stmt.query_map([], |row| {
        Ok(IntentRow {
            channel: row.get(0)?,
            external_id: row.get(1)?,
            session_id: row.get(2)?,
            last_user_reply_at: parse_ts(row.get(3)?),
            last_intent_at: parse_ts(row.get(4)?),
            today_count: row.get(5)?,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::db::session;

    fn add_user(db: &Database, channel: &str, external_id: &str) -> String {
        let s = session::create(db, "").unwrap();
        let now = Utc::now().to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![channel, external_id, s.id, now, now],
            )
            .unwrap();
        s.id
    }

    #[test]
    fn touch_and_record_roundtrip() {
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "telegram", "100");
        touch_user_reply(&db, "telegram", "100", &sid).unwrap();

        let rows = list_rows(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].last_user_reply_at.is_some());
        assert_eq!(rows[0].today_count, 0);

        record_intent_send(&db, "telegram", "100", &sid).unwrap();
        record_intent_send(&db, "telegram", "100", &sid).unwrap();
        let rows = list_rows(&db).unwrap();
        assert_eq!(rows[0].today_count, 2);
        assert!(rows[0].last_intent_at.is_some());
    }
}