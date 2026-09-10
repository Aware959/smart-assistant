//! 外部渠道会话映射：把第三方平台的对话（Telegram chat 等）映射到内部 session。
//!
//! 每个外部对话拥有独立 session，多轮上下文互不串扰；映射持久化在
//! `channel_sessions` 表中，重启进程后继续沿用。

use chrono::Utc;

use crate::db::session::Session;
use crate::db::Database;
use crate::error::Result;

/// 取外部对话对应的 session；不存在则新建 session 并建立映射。
pub fn get_or_create_session(
    db: &Database,
    channel: &str,
    external_id: &str,
) -> Result<Session> {
    if let Some(session_id) = find_session_id(db, channel, external_id)? {
        if let Some(session) = crate::db::session::get(db, &session_id)? {
            touch(db, channel, external_id)?;
            return Ok(session);
        }
        // 映射指向的 session 已被删除（如前端删会话）：重建映射。
        delete_mapping(db, channel, external_id)?;
    }

    let session = crate::db::session::create(db, "")?;
    let now = Utc::now().to_rfc3339();
    db.conn().execute(
        "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        rusqlite::params![channel, external_id, session.id, now],
    )?;
    Ok(session)
}

/// 重置外部对话的会话：下一次消息从空白上下文开始（旧 session 保留，可在前端查看）。
pub fn reset_session(db: &Database, channel: &str, external_id: &str) -> Result<Session> {
    delete_mapping(db, channel, external_id)?;
    get_or_create_session(db, channel, external_id)
}

fn find_session_id(db: &Database, channel: &str, external_id: &str) -> Result<Option<String>> {
    let mut stmt = db.conn().prepare(
        "SELECT session_id FROM channel_sessions WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![channel, external_id], |row| {
        row.get::<_, String>(0)
    })?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

fn touch(db: &Database, channel: &str, external_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    db.conn().execute(
        "UPDATE channel_sessions SET updated_at = ?1 WHERE channel = ?2 AND external_id = ?3",
        rusqlite::params![now, channel, external_id],
    )?;
    Ok(())
}

fn delete_mapping(db: &Database, channel: &str, external_id: &str) -> Result<()> {
    db.conn().execute(
        "DELETE FROM channel_sessions WHERE channel = ?1 AND external_id = ?2",
        rusqlite::params![channel, external_id],
    )?;
    Ok(())
}
