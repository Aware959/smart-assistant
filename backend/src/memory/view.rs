//! 记忆能力直读/写门面（供 web / 管理端等外部入口使用）。

use crate::core::types::{MemoryHit, MemoryRecord};
use crate::db::Database;
use crate::error::Result;
use crate::memory::store;

/// 语义检索记忆。
pub fn search_memory(db: &Database, query: &str, limit: u32) -> Result<Vec<MemoryHit>> {
    store::validate_query(query)?;
    let hits = store::search(db, query, limit as usize)?;
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
    let memory = store::store(db, content, memory_type, "core", message_id)?;
    Ok(memory.id)
}

/// 列出全部记忆。
pub fn list_memories(db: &Database, limit: u32) -> Result<Vec<MemoryRecord>> {
    let memories = store::list_all(db, limit as usize)?;
    Ok(memories.iter().map(|m| m.into()).collect())
}

/// 删除一条记忆。
pub fn delete_memory(db: &Database, id: &str) -> Result<()> {
    store::remove(db, id)
}
