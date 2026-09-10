use crate::db::memory as db_memory;
use crate::db::Database;
use crate::db::memory::Memory;
use crate::embedding;
use crate::error::{Result, SqlError};

/// 将一段内容沉淀为记忆（事实）：向量化 → 存入 memories 与 memory_vectors。
///
/// `message_id` 记录该记忆来源的对话消息（手动添加时传 None）。
pub fn store(
    db: &Database,
    content: &str,
    memory_type: &str,
    message_id: Option<&str>,
) -> Result<Memory> {
    let vector = embedding::embed_text(content)?;
    let memory = Memory::new(content.to_string(), memory_type.to_string(), message_id.map(String::from));
    db_memory::create(db, &memory, &vector)?;
    Ok(memory)
}

/// 更新记忆内容；若提供了新文本，会重新向量化。若 new_embedding 为空则仅更新内容。
pub fn update(db: &Database, id: &str, new_content: Option<&str>, reembed: bool) -> Result<()> {
    if let Some(content) = new_content {
        let new_embedding = if reembed {
            Some(embedding::embed_text(content)?)
        } else {
            None
        };
        let vec = new_embedding.unwrap_or_default();
        db_memory::update(db, id, content, &vec)?;
    }
    Ok(())
}

/// 语义检索记忆。
pub fn search(db: &Database, query: &str, limit: usize) -> Result<Vec<(Memory, f32)>> {
    let vector = embedding::embed_text(query)?;
    db_memory::search_similar(db, &vector, limit)
}

/// 删除一条记忆（含向量）。
pub fn remove(db: &Database, id: &str) -> Result<()> {
    db_memory::delete(db, id)?;
    Ok(())
}

/// 列出全部记忆。
pub fn list_all(db: &Database, limit: usize) -> Result<Vec<Memory>> {
    db_memory::list(db, limit)
}

/// 获取单条记忆。
pub fn get(db: &Database, id: &str) -> Result<Memory> {
    db_memory::get(db, id)
}

/// 防止空查询导致的 API 浪费。
pub fn validate_query(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        return Err(SqlError::Config("查询不能为空".to_string()));
    }
    Ok(())
}