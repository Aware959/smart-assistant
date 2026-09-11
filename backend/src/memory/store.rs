use crate::db::memory as db_memory;
use crate::db::Database;
use crate::db::memory::Memory;
use crate::embedding;
use crate::error::{Result, SqlError};

/// 将一段内容沉淀为记忆（事实）：向量化 → 存入 memories 与 memory_vectors。
///
/// `message_id` 记录该记忆来源的对话消息（手动添加时传 None）。
///
/// 向量签名不匹配（维度变更未重建）时退化为只存 [`memories`] 内容、不写向量；
/// 后续由 `smart-assistant-memory rebuild` 统一补齐。
pub fn store(
    db: &Database,
    content: &str,
    memory_type: &str,
    message_id: Option<&str>,
) -> Result<Memory> {
    let memory = Memory::new(content.to_string(), memory_type.to_string(), message_id.map(String::from));
    if db.vec_status()?.search_usable() {
        let vector = embedding::embed_text(content)?;
        db_memory::create(db, &memory, &vector)?;
    } else {
        db_memory::create_record(db, &memory)?;
    }
    Ok(memory)
}

/// 更新记忆内容；若提供了新文本，会重新向量化。若 new_embedding 为空则仅更新内容。
pub fn update(db: &Database, id: &str, new_content: Option<&str>, reembed: bool) -> Result<()> {
    if let Some(content) = new_content {
        let new_embedding = if reembed && db.vec_status()?.search_usable() {
            Some(embedding::embed_text(content)?)
        } else {
            None
        };
        let vec = new_embedding.unwrap_or_default();
        db_memory::update(db, id, content, &vec)?;
    }
    Ok(())
}

/// 语义检索记忆。向量签名不匹配时返回空结果（检索暂停，等待确认重建）。
pub fn search(db: &Database, query: &str, limit: usize) -> Result<Vec<(Memory, f32)>> {
    if !db.vec_status()?.search_usable() {
        return Ok(Vec::new());
    }
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

/// 重建全部记忆的向量：重建 `memory_vectors` 表后按 `memories.content` 逐条重新向量化。
///
/// - **只**读写 `memory_vectors`，memories 的 content/created_at/updated_at 完全不碰；
/// - 单条向量化失败仅记录日志，不中断整体重建；
/// - 返回成功写入的向量条数。
pub fn rebuild_all(db: &Database) -> Result<usize> {
    let status = db.vec_status()?;
    db.rebuild_vectors(status.configured_dim, &status.configured_model)?;

    let memories = db_memory::list(db, usize::MAX)?;
    let mut done = 0usize;
    for m in &memories {
        match embedding::embed_text(&m.content) {
            Ok(vector) => {
                db_memory::insert_vector(db, &m.id, &vector)?;
                done += 1;
            }
            Err(e) => tracing::warn!("记忆 {} 向量化失败，跳过: {e}", m.id),
        }
    }
    Ok(done)
}

/// 防止空查询导致的 API 浪费。
pub fn validate_query(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        return Err(SqlError::Config("查询不能为空".to_string()));
    }
    Ok(())
}