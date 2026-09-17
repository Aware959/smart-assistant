//! 服务层记忆能力：语义检索 / 手动添加 / 列出 / 删除。
//!
//! 记忆的业务实现（提取、存储、召回）在 [`crate::memory`]，本层只做视图映射与门面。

use crate::core::types::{MemoryHit, MemoryRecord};
use crate::db::Database;
use crate::error::Result;
use crate::memory;

use super::records::memory_to_record;

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
