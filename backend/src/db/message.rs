use chrono::{DateTime, Utc};
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
    /// assistant 轮携带的工具调用列表（JSON 数组），持久化工具往返用。
    pub tool_calls: Option<String>,
    /// `role='assistant'` 且 `tool_call_id` 非空时，表示这是某次工具调用的结果，
    /// 加载历史时还原为 genai 的 role='tool'。
    pub tool_call_id: Option<String>,
    /// 工具结果对应的工具名（部分 provider 回填结果时要求）。
    pub tool_name: Option<String>,
}

/// 工具相关字段（工具往返持久化用），可整体为空。
#[derive(Debug, Clone, Default)]
pub struct ToolMeta {
    /// assistant 轮的工具调用列表（JSON 数组）。
    pub tool_calls: Option<String>,
    /// 工具结果行的调用 id（非空即代表该消息是某次工具调用的结果）。
    pub tool_call_id: Option<String>,
    /// 工具结果对应的工具名。
    pub tool_name: Option<String>,
}

impl Message {
    pub fn new(session_id: String, role: String, content: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id,
            role,
            content,
            created_at: Utc::now().to_rfc3339(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
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
    create_full(db, session_id, role, content, at, &ToolMeta::default())
}

/// 带工具字段落库：工具往返（assistant 轮的工具调用列表、tool 结果的调用 id 与工具名）。
///
/// `meta.tool_calls` 为 `assistant` 轮的工具调用 JSON 数组；`meta.tool_call_id` /
/// `meta.tool_name` 为工具结果行使用（结果沿用 `role='assistant'` + `tool_call_id`
/// 标记，避免改动 messages 的 role CHECK 约束）。三者均可空。
pub fn create_full(
    db: &Database,
    session_id: &str,
    role: &str,
    content: &str,
    at: chrono::DateTime<Utc>,
    meta: &ToolMeta,
) -> Result<Message> {
    let message = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        content: content.to_string(),
        created_at: at.to_rfc3339(),
        tool_calls: meta.tool_calls.clone(),
        tool_call_id: meta.tool_call_id.clone(),
        tool_name: meta.tool_name.clone(),
    };
    db.conn().execute(
        "INSERT INTO messages (id, session_id, role, content, created_at, tool_calls, tool_call_id, tool_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            message.id,
            message.session_id,
            message.role,
            message.content,
            message.created_at,
            message.tool_calls,
            message.tool_call_id,
            message.tool_name
        ],
    )?;
    Ok(message)
}

/// 所有列名（含工具字段），各 SELECT 共用以保证列序一致。
const COLS: &str = "id, session_id, role, content, created_at, tool_calls, tool_call_id, tool_name";

pub fn get(db: &Database, id: &str) -> Result<Option<Message>> {
    let mut stmt = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM messages WHERE id = ?1"))?;
    let mut rows = stmt.query_map([id], map_message_row)?;
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

/// 取某会话在 `since`（UTC，含）之后的消息，按时间正序。
/// created_at 统一以 `to_rfc3339()` 落库，文本序即时间序。
pub fn list_since(db: &Database, session_id: &str, since: DateTime<Utc>) -> Result<Vec<Message>> {
    let mut stmt = db.conn().prepare(&format!(
        "SELECT {COLS} FROM messages WHERE session_id = ?1 AND created_at >= ?2
         ORDER BY created_at ASC, rowid ASC"
    ))?;
    let rows = stmt.query_map(
        rusqlite::params![session_id, since.to_rfc3339()],
        map_message_row,
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// 按会话取最近 `limit` 条消息，返回时仍按时间正序。
pub fn list_recent(db: &Database, session_id: &str, limit: usize) -> Result<Vec<Message>> {
    list_range(db, session_id, limit, true)
}

/// 一次性读出某会话全部消息（按时间正序），供内存运行时启动/首碰水合。
pub fn list_all(db: &Database, session_id: &str) -> Result<Vec<Message>> {
    let mut stmt = db.conn().prepare(&format!(
        "SELECT {COLS} FROM messages WHERE session_id = ?1
         ORDER BY created_at ASC, rowid ASC"
    ))?;
    let rows = stmt.query_map([session_id], map_message_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// 该会话最近一条用户消息的时刻（无用户消息返回 None），用于时间世界模型。
pub fn latest_user_at(db: &Database, session_id: &str) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT created_at FROM messages
         WHERE session_id = ?1 AND role = 'user'
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![session_id], |r| r.get::<_, String>(0))?;
    match rows.next() {
        Some(Ok(ts)) => Ok(DateTime::parse_from_rfc3339(&ts)
            .ok()
            .map(|d| d.with_timezone(&Utc))),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

fn list_range(db: &Database, session_id: &str, limit: usize, recent: bool) -> Result<Vec<Message>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let (by, rowid) = if recent {
        ("DESC", "DESC")
    } else {
        ("ASC", "ASC")
    };
    let sql = format!(
        "SELECT {COLS} FROM messages WHERE session_id = ?1
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
        tool_calls: row.get(5)?,
        tool_call_id: row.get(6)?,
        tool_name: row.get(7)?,
    })
}

pub fn delete_for_session(db: &Database, session_id: &str) -> Result<()> {
    db.conn()
        .execute("DELETE FROM messages WHERE session_id = ?1", [session_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use chrono::{Duration, Utc};

    fn session_id(db: &Database) -> String {
        crate::db::session::create(db, "").unwrap().id
    }

    #[test]
    fn list_since_respects_boundary_and_order() {
        let db = Database::in_memory().unwrap();
        let sid = session_id(&db);
        let base = Utc::now();
        create_at(&db, &sid, "user", "早", base - Duration::days(1)).unwrap();
        create_at(&db, &sid, "user", "中", base).unwrap();
        create_at(&db, &sid, "user", "晚", base + Duration::minutes(5)).unwrap();

        let got = list_since(&db, &sid, base).unwrap();
        let contents: Vec<&str> = got.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["中", "晚"],
            "仅返回 since 之后的并按时间正序"
        );

        let empty = list_since(&db, &sid, base + Duration::hours(1)).unwrap();
        assert!(empty.is_empty());
    }
}
