//! 失败对话轮次的持久化重试队列。

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::error::Result;

/// 一次失败对话轮次的待重试记录。
///
/// 复用已经落库的用户消息（`user_message_id`），避免重复落库；`attempts` 记录已重试次数，
/// `next_attempt_at` 控制下次扫表重试的时刻（RFC3339）。队列在 DB 里，进程重启不丢。
#[derive(Debug, Clone)]
pub struct PendingRetry {
    pub id: String,
    pub session_id: String,
    pub chat_id: String,
    pub user_message_id: String,
    pub content: String,
    pub user_time: Option<String>,
    pub attempts: i64,
    pub next_attempt_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, session_id, chat_id, user_message_id, content, user_time, attempts, next_attempt_at, created_at";

/// 入队一条待重试记录。
pub fn enqueue(db: &Database, row: &PendingRetry) -> Result<()> {
    db.conn().execute(
        "INSERT INTO pending_retry \
         (id, session_id, chat_id, user_message_id, content, user_time, attempts, next_attempt_at, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            row.id,
            row.session_id,
            row.chat_id,
            row.user_message_id,
            row.content,
            row.user_time,
            row.attempts,
            row.next_attempt_at.to_rfc3339(),
            row.created_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// 取所有已到重试时刻（`next_attempt_at <= now`）的记录，按时间正序。
pub fn due(db: &Database, now: &DateTime<Utc>) -> Result<Vec<PendingRetry>> {
    let mut stmt = db.conn().prepare(&format!(
        "SELECT {COLUMNS} FROM pending_retry \
         WHERE next_attempt_at <= ?1 ORDER BY next_attempt_at"
    ))?;
    let rows = stmt
        .query_map([now.to_rfc3339()], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 推进重试计数并设定下次重试时刻（用于"本次重试仍失败、但还没到上限"）。
pub fn mark_attempted(db: &Database, id: &str, next_attempt_at: &DateTime<Utc>) -> Result<()> {
    db.conn().execute(
        "UPDATE pending_retry SET attempts = attempts + 1, next_attempt_at = ?1 WHERE id = ?2",
        rusqlite::params![next_attempt_at.to_rfc3339(), id],
    )?;
    Ok(())
}

/// 删除一条记录（重试成功、或放弃重试时调用）。
pub fn remove(db: &Database, id: &str) -> Result<()> {
    db.conn()
        .execute("DELETE FROM pending_retry WHERE id = ?1", [id])?;
    Ok(())
}

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<PendingRetry> {
    let user_time: Option<String> = r.get(5)?;
    let next_attempt_at = parse_utc(r.get::<_, String>(7)?)?;
    let created_at = parse_utc(r.get::<_, String>(8)?)?;
    Ok(PendingRetry {
        id: r.get(0)?,
        session_id: r.get(1)?,
        chat_id: r.get(2)?,
        user_message_id: r.get(3)?,
        content: r.get(4)?,
        user_time,
        attempts: r.get(6)?,
        next_attempt_at,
        created_at,
    })
}

fn parse_utc(s: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}
