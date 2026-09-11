//! 服务层：在 `db::*` 之上提供会话 / 消息 / 记忆的管理能力，
//! 并产出 FFI / HTTP 可序列化的视图记录类型。不持有状态，全部以 `&Database` 为入参。

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;
use crate::memory;

// ---------- 视图（记录）类型 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    /// 记忆层级：short / intent / core。
    pub tier: String,
    /// 过期时间（RFC3339）；None 表示长期记忆。
    pub expires_at: Option<String>,
    pub message_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    pub tier: String,
    pub created_at: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

pub(crate) fn memory_to_record(m: &crate::db::memory::Memory) -> MemoryRecord {
    MemoryRecord {
        id: m.id.clone(),
        content: m.content.clone(),
        memory_type: m.memory_type.clone(),
        tier: m.tier.clone(),
        expires_at: m.expires_at.clone(),
        message_id: m.message_id.clone(),
        created_at: m.created_at.clone(),
        updated_at: m.updated_at.clone(),
    }
}

fn session_to_record(s: crate::db::session::Session) -> SessionRecord {
    SessionRecord {
        id: s.id,
        title: s.title,
        created_at: s.created_at,
        updated_at: s.updated_at,
    }
}

fn message_to_record(m: crate::db::message::Message) -> MessageRecord {
    MessageRecord {
        id: m.id,
        session_id: m.session_id,
        role: m.role,
        content: m.content,
        created_at: m.created_at,
    }
}

// ---------- 记忆 ----------

/// 语义检索记忆。
pub fn search_memory(db: &Database, query: &str, limit: u32) -> Result<Vec<MemoryHit>> {
    memory::store::validate_query(query)?;
    let hits = memory::store::search(db, query, limit as usize)?;
    Ok(hits
        .into_iter()
        .map(|(m, score)| MemoryHit {
            id: m.id,
            content: m.content,
            memory_type: m.memory_type,
            tier: m.tier,
            created_at: m.created_at,
            score,
        })
        .collect())
}

/// 手动添加一条记忆（无来源消息），默认按 core 长期记忆处理，返回记忆 id。
pub fn add_memory(
    db: &Database,
    content: &str,
    memory_type: &str,
    message_id: Option<&str>,
) -> Result<String> {
    let memory = memory::store::store(db, content, memory_type, "core", message_id)?;
    Ok(memory.id)
}

/// 列出全部记忆。
pub fn list_memories(db: &Database, limit: u32) -> Result<Vec<MemoryRecord>> {
    let memories = memory::store::list_all(db, limit as usize)?;
    Ok(memories.iter().map(memory_to_record).collect())
}

/// 删除一条记忆。
pub fn delete_memory(db: &Database, id: &str) -> Result<()> {
    memory::store::remove(db, id)
}

// ---------- 会话 / 消息 ----------

/// 新建会话，返回会话 id。
pub fn create_session(db: &Database, title: &str) -> Result<String> {
    let session = crate::db::session::create(db, title)?;
    Ok(session.id)
}

/// 列出会话（按最近更新倒序）。
pub fn list_sessions(db: &Database, limit: u32) -> Result<Vec<SessionRecord>> {
    Ok(crate::db::session::list(db, limit as usize)?
        .into_iter()
        .map(session_to_record)
        .collect())
}

/// 列出某会话内的消息（时间正序）。
pub fn list_messages(db: &Database, session_id: &str, limit: u32) -> Result<Vec<MessageRecord>> {
    Ok(crate::db::message::list_by_session(db, session_id, limit as usize)?
        .into_iter()
        .map(message_to_record)
        .collect())
}

/// 删除会话及其全部消息（记忆保留，来源 message_id 置空）。
pub fn delete_session(db: &Database, id: &str) -> Result<()> {
    crate::db::session::delete(db, id)
}
