use std::sync::{Arc, Mutex};

use chrono::Utc;

use crate::config::Config;
use crate::core::ports::MemoryStore;
use crate::core::types::{Extraction, MemoryHit, MemoryRecord};
use crate::db::memory as db_memory;
use crate::db::Database;
use crate::db::memory::Memory;
use crate::llm::embedding;
use crate::error::{Result, SqlError};

/// 记忆适配器：把 [`MemoryStore`] 端口接到本模块实现函数上。
/// 数据库句柄由宿主持有并以 `Arc<Mutex<Database>>` 注入，自身不拥有连接。
pub struct Store {
    db: Arc<Mutex<Database>>,
}

impl Store {
    pub fn new(db: Arc<Mutex<Database>>) -> Self {
        Self { db }
    }

    fn lock_db(&self) -> std::sync::MutexGuard<'_, Database> {
        crate::db::lock_db(&self.db)
    }
}

/// 记忆记录 → 可序列化视图记录（FFI / HTTP 用）。
impl From<&Memory> for MemoryRecord {
    fn from(m: &Memory) -> Self {
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
}

impl MemoryStore for Store {
    fn extract(&self, text: &str) -> Result<Extraction> {
        let e = crate::memory::extraction::extract_from_text(text)?;
        Ok(Extraction {
            is_memory: e.is_memory,
            content: e.memory_content,
            memory_type: e.memory_type,
            tier: e.tier,
            relation: e.relation,
        })
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
        validate_query(query)?;
        let hits = search(&self.lock_db(), query, limit)?;
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

    fn store(
        &self,
        content: &str,
        memory_type: &str,
        tier: &str,
        message_id: Option<&str>,
    ) -> Result<MemoryRecord> {
        let m = store(&self.lock_db(), content, memory_type, tier, message_id)?;
        Ok(MemoryRecord::from(&m))
    }
}

/// 将一段内容沉淀为记忆：向量化 → 存入 memories 与 memory_vectors。
///
/// - **去重**：与库中某条距离 ≤ `memory_dedup_threshold` 时视为同一事实，
///   只刷新 updated_at 不新增（避免同一件事反复沉淀）；
/// - **分层**：`tier` 决定存活期——`short` 按 `memory_short_ttl_days`，
///   `intent` 按 `memory_intent_ttl_days` 写入 expires_at，`core` 永不过期；
/// - `message_id` 记录该记忆来源的对话消息（手动添加时传 None）。
///
/// 向量签名不匹配（维度变更未重建）时退化为只存 [`memories`] 内容、不写向量；
/// 后续由 `smart-assistant-memory rebuild` 统一补齐。
pub fn store(
    db: &Database,
    content: &str,
    memory_type: &str,
    tier: &str,
    message_id: Option<&str>,
) -> Result<Memory> {
    let cfg = Config::get();
    let usable = db.vec_status()?.search_usable();

    // 向量化一次，同时用于去重判定与正式写入。
    let mut vector = Vec::new();
    let mut dedup_hits: Vec<(Memory, f32)> = Vec::new();
    if usable {
        vector = embedding::embed_text(content)?;
        dedup_hits = db_memory::search_similar(db, &vector, 1)?;
    }
    if let Some((existing, distance)) = dedup_hits.into_iter().next() {
        if distance <= cfg.memory_dedup_threshold {
            db_memory::touch(db, &existing.id)?;
            tracing::debug!(
                id = %existing.id,
                distance,
                "记忆去重：命中现有条目，刷新 updated_at 不新增"
            );
            return Ok(existing);
        }
    }

    let ttl: i64 = match tier {
        "short" => cfg.memory_short_ttl_days,
        "intent" => cfg.memory_intent_ttl_days,
        _ => 0,
    };
    let expires_at = (ttl > 0)
        .then(|| (Utc::now() + chrono::Duration::days(ttl)).to_rfc3339());

    let memory = Memory::new(
        content.to_string(),
        memory_type.to_string(),
        tier.to_string(),
        message_id.map(String::from),
        expires_at,
    );

    if usable && !vector.is_empty() {
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

/// 语义检索记忆。**只返回未过期且距离 ≤ `memory_recall_threshold` 的条目**；
/// 向量签名不匹配时返回空结果（检索暂停，等待确认重建）。
pub fn search(db: &Database, query: &str, limit: usize) -> Result<Vec<(Memory, f32)>> {
    if !db.vec_status()?.search_usable() {
        return Ok(Vec::new());
    }
    let vector = embedding::embed_text(query)?;
    let threshold = Config::get().memory_recall_threshold;
    Ok(db_memory::search_similar(db, &vector, limit)?
        .into_iter()
        .filter(|(_, distance)| *distance <= threshold)
        .collect())
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