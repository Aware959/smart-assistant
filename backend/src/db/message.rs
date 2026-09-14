use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

impl Message {
    pub fn new(session_id: String, role: String, content: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id,
            role,
            content,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

pub fn create(db: &Database, session_id: &str, role: &str, content: &str) -> Result<Message> {
    create_at(db, session_id, role, content, Utc::now())
}

/// 以给定时刻落库（消息真实发出时间，如通道报文里的时间戳），用于时间世界模型。
pub fn create_at(
    db: &Database,
    session_id: &str,
    role: &str,
    content: &str,
    at: chrono::DateTime<Utc>,
) -> Result<Message> {
    let message = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        content: content.to_string(),
        created_at: at.to_rfc3339(),
    };
    db.conn().execute(
        "INSERT INTO messages (id, session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            message.id,
            message.session_id,
            message.role,
            message.content,
            message.created_at
        ],
    )?;
    Ok(message)
}

pub fn get(db: &Database, id: &str) -> Result<Option<Message>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, session_id, role, content, created_at FROM messages WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |row| {
        Ok(Message {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    if let Some(row) = rows.next() {
        Ok(Some(row?))
    } else {
        Ok(None)
    }
}

/// 按会话取消息，按时间正序（时间早的在前）。
pub fn list_by_session(db: &Database, session_id: &str, limit: usize) -> Result<Vec<Message>> {
    list_range(db, session_id, limit, false)
}

/// 按会话取最近的 `limit` 条消息，返回时仍按时间正序。
pub fn list_recent(db: &Database, session_id: &str, limit: usize) -> Result<Vec<Message>> {
    list_range(db, session_id, limit, true)
}

fn list_range(db: &Database, session_id: &str, limit: usize, recent: bool) -> Result<Vec<Message>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let (by, rowid) = if recent { ("DESC", "DESC") } else { ("ASC", "ASC") };
    let sql = format!(
        "SELECT id, session_id, role, content, created_at
         FROM messages WHERE session_id = ?1
         ORDER BY created_at {by}, rowid {rowid}
         LIMIT ?2"
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![session_id, limit as i64], map_message_row)?;
    let mut result: Vec<Message> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if recent {
        result.reverse();
    }
    Ok(result)
}

fn map_message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        created_at: row.get(4)?,
    })
}

pub fn delete_for_session(db: &Database, session_id: &str) -> Result<()> {
    db.conn().execute(
        "DELETE FROM messages WHERE session_id = ?1",
        [session_id],
    )?;
    Ok(())
}