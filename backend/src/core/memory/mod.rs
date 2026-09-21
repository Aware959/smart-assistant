//! 记忆：AI 的记忆资产 = **关于对方的**（可召回事实）+ **关于自己的**（自身状态）。
//!
//! - [`user_memory_extraction`] + 本模块 `Store`（记忆能力的具体实现，见下方
//!   [`Store`] 的 `extract` / `search` / `store` 内在方法，以及 [`store`] /
//!   [`search`] / [`rebuild_all`] 自由函数）：关于对方的记忆——
//!   LLM 判定/事实化 + 向量存取（分级、去重、过期、长程语义召回）。该子域是
//!   "用户记忆"的唯一入口，适配器职责就是**依赖组装与接口暴露**，收在 `mod.rs`；
//! - [`current_relation`] / [`current_phase`]：关于 AI 自己的结构化状态（纯值 +
//!   演化规则，值本身不由本层持有）。**aiworld 已整体拆出等待重新设计**：情绪 /
//!   自我档案 / 快照渲染与内存世界运行时已移除，这两份实现先留着，新设计定稿后在
//!   `runtime` 重新装配接线。
//!
//! 两域存储语义不同：用户记忆可向量召回、量多；自身状态是结构化运行量、量少、
//! 全量常驻直读。不涉及会话/消息/图谱。

pub mod current_phase;
pub mod current_relation;
pub mod user_memory_extraction;

// ---------- 记忆域值类型（跨能力层契约，纯内核值，不带序列化） ----------

/// 记忆判定结果：一次 LLM 调用同时产出"要不要记 / 记什么 / 关系事件"。
/// 与记忆能力层解耦——内核只消费这套可移植值，不感知具体解析实现。
#[derive(Debug, Clone)]
pub struct Extraction {
    /// 是否把这条用户消息沉淀为记忆（事实）。
    pub is_memory: bool,
    /// 值得记住时给出的事实化内容（无则由原文替代）。
    pub content: Option<String>,
    /// 事实类型，默认 fact。
    pub memory_type: String,
    /// 记忆层级：short / intent / core。
    pub tier: String,
    /// 本条消息对两人关系的影响事件标签（neutral / warm / ambig / conflict / reconcile）。
    pub relation: String,
}

impl Extraction {
    /// 取出可落库的事实文本：优先用 LLM 事实化内容，否则回退原文。
    pub fn content_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.content
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or(fallback)
    }
}

/// 记忆视图记录（内核向宿主交付的记忆快照）。
#[derive(Debug, Clone)]
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

/// 语义检索命中的记忆（含相关度分数）。
#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    pub tier: String,
    pub created_at: String,
    pub score: f32,
}

// ---------- 用户记忆适配器（依赖组装 + 端口实现） ----------

use std::sync::{Arc, Mutex};

use chrono::Utc;

use crate::config::Config;
use crate::db::memory as db_memory;
use crate::db::memory::Memory;
use crate::db::Database;
use crate::error::{Result, SqlError};
use crate::llm::embedding;

/// 记忆适配器（记忆能力的具体实现）：判定 / 检索 / 沉淀。
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

    /// 判定"是否值得沉淀记忆"，并产出记忆事实与关系事件标签。
    /// 内部按两步调用（先出标签，判定值得记才做事实化），调用方不感知这个细节。
    pub fn extract(&self, text: &str) -> Result<Extraction> {
        let e = user_memory_extraction::extract_from_text(text)?;
        Ok(Extraction {
            is_memory: e.is_memory,
            content: e.memory_content,
            memory_type: e.memory_type,
            tier: e.tier,
            relation: e.relation,
        })
    }

    /// 语义检索记忆（已过滤相关度、剔除过期条目）。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
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

    /// 沉淀一段记忆为事实（含去重 / 分层），返回可序列化的记忆记录。
    pub fn store(
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

// ---------- 用户记忆自由函数（inherent Store 已在上方实现，以下为无状态辅助） ----------

/// 将一段内容沉淀为记忆：向量化 → 存入 memories 与 memory_vectors。
///
/// - **去重**：与库中某条距离 ≤ `memory_dedup_threshold` 时视为同一事实，
///   只刷新 updated_at 不新增（避免同一件事反复沉淀）；
/// - **分层**：`tier` 决定存活期——`short` 按 `memory_short_ttl_days`，
///   `intent` 按 `memory_intent_ttl_days` 写入 expires_at，`core` 永不过期；
/// - `message_id` 记录该记忆来源的对话消息（手动添加时传 None）。
///
/// 向量签名不匹配（维度变更未重建）时退化为只存 `memories` 内容、不写向量；
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