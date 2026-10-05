//! 记忆：AI 的记忆资产 = **关于对方的**（可召回事实）+ **关于自己的**（自身状态）。
//!
//! - 本模块 `Store`（记忆能力的具体实现，见 [`Store`] 的 `search` / `store` 内在方法，
//!   以及 [`store`] / [`search`] / [`rebuild_all`] 自由函数）：关于对方的记忆——
//!   模型主动写入 + 向量存取（分级、去重、原地取代、过期、长程语义召回）。该子域是
//!   "用户记忆"的唯一入口，适配器职责就是**依赖组装与接口暴露**，收在 `mod.rs`；
//! - **不再有"关于 AI 自己"的结构化状态域**：原先的 `current_relation`（亲密度 × 信任度）
//!   与 `current_phase`（时段）从未接线到任何生产路径，且语义与记忆域重叠，已整体删除。
//!   需要"关系感"时走记忆（`kind = trait / preference`）与人设（`self_profile`）。
//!
//! 不涉及会话/消息/图谱。
//!
//! ---
//!
//! ## 什么进得来：种类白名单 + 模型拿着整段对话自己判断
//!
//! 进库的门槛是一份**可枚举的白名单** [`MemoryKind`]（trait / preference / routine /
//! plan）——像 AI 人设抽成 `self_profile` KV 表一样，把"什么值得记"从散落的散文变成
//! 四项各有名字、各带规则的清单。装不进去的就不写。
//!
//! 至于"这一条够不够格"，本模块**不做**判定——那需要上下文，而判定过去被放在了只有
//! 单条消息的旁路里，于是它只能看见"对方自称只是傲娇"这种半截话，写出一堆悬空指代
//! 和问句。现在写入由模型在工具循环里发起（见 `llm::tool` 的 `remember`）：
//! 它看得见前后文，能自己消解"我搬家了"指的是哪件事，也知道这句话值不值得留。
//!
//! 剩下的机械判断留在这里，且只有机械的：
//! - **去重**：与库中某条高度近似 → 原地改写那一条，不新增；
//! - **原地取代**：模型显式指明被取代的记忆 → 改写那一条（见 [`WriteKind`]）；
//! - **寿命**：种类决定默认窗口，强时间锚能算出精确到期日（见 [`resolve_lifetime`]）。
//!
//! [`MemoryKind`] 由模型选，[`resolve_lifetime`] 由代码算——分工的理由见那里。

mod temporal;

// ---------- 记忆域值类型（跨能力层契约，纯内核值，不带序列化） ----------

pub use crate::db::memory::MemoryKind;

/// 记忆视图记录（内核向宿主交付的记忆快照）。
#[derive(Debug, Clone)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    /// 种类（[`MemoryKind`]）；同时决定默认寿命。
    pub memory_type: MemoryKind,
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
    pub memory_type: MemoryKind,
    pub created_at: String,
    /// 余弦相似度，越大越像。
    pub score: f32,
}

/// 一次写入是新增了一行，还是改写了已存在的一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// 库中没有相近记忆、也没被显式指定 → 新增一行。
    Created,
    /// 改写了已存在的行（模型显式 `supersedes`，或相似度去重命中）。
    Revised,
}

/// 写入结果：记忆记录 + 本次写入的性质。
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub memory: MemoryRecord,
    pub kind: WriteKind,
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

/// 记忆适配器（记忆能力的具体实现）：检索 / 写入。
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
                created_at: m.created_at,
                score,
            })
            .collect())
    }

    /// 写入一条记忆。`kind` 是模型判定的种类（决定默认寿命）；`supersedes` 指向要
    /// 改写的既有记忆（来自 [`Store::search`] 的 `id`），为 None 时先按相似度去重，
    /// 命中相近记忆同样原地改写。
    pub fn store(
        &self,
        content: &str,
        kind: MemoryKind,
        supersedes: Option<&str>,
        message_id: Option<&str>,
    ) -> Result<WriteOutcome> {
        store(&self.lock_db(), content, kind, supersedes, message_id)
    }
}

/// 记忆记录 → 可序列化视图记录（FFI / HTTP 用）。
impl From<&Memory> for MemoryRecord {
    fn from(m: &Memory) -> Self {
        MemoryRecord {
            id: m.id.clone(),
            content: m.content.clone(),
            memory_type: m.memory_type,
            expires_at: m.expires_at.clone(),
            message_id: m.message_id.clone(),
            created_at: m.created_at.clone(),
            updated_at: m.updated_at.clone(),
        }
    }
}

// ---------- 用户记忆自由函数（inherent Store 已在上方实现，以下为无状态辅助） ----------

/// 余弦距离 → 相似度。只有一处含义，别在各处散写 `1.0 - d`。
fn similarity(distance: f32) -> f32 {
    1.0 - distance
}

/// 写入一段记忆。
///
/// 落哪一行由三步决定，先命中先算：
///
/// 1. **`supersedes` 显式取代**：模型指了 id 就改写那一行——这条最优先，因为只有
///    看过上下文的模型知道"我搬到上海了"取代的是"对方住在北京"，向量相似度猜不出
///    这种关系（两句字面几乎不重合）。id 不存在时直接报错让模型重试，不静默降级
///    成新增，否则库里会同时留着矛盾的两条；
/// 2. **相似度去重**：与库中某条余弦相似度 ≥ `memory_dedup_min_similarity` → 改写
///    那一条。改写而不是"只刷新时间"，因为模型刚给出的是它斟酌后的说法，比库里
///    可能存在的旧措辞更准；
/// 3. **都没命中**：新增一行。
///
/// 新增与改写的行都过 [`resolve_lifetime`]：种类给出默认窗口，强时间锚把它覆盖成
/// 精确到期日（"下周要考试"不该被当成长期事实）。
///
/// 向量签名不匹配（维度或度量变更未重建）时退化为只存 `memories` 内容、不写向量，
/// 后续由 `sa-mem rebuild` 统一补齐。
pub fn store(
    db: &Database,
    content: &str,
    kind: MemoryKind,
    supersedes: Option<&str>,
    message_id: Option<&str>,
) -> Result<WriteOutcome> {
    let cfg = Config::get();
    let usable = db.vec_search_usable()?;

    // 向量化一次，同时用于去重判定与正式写入。
    let vector = if usable {
        embedding::embed_text(content)?
    } else {
        Vec::new()
    };

    let target = match supersedes {
        Some(id) => {
            let old = db_memory::get(db, id)
                .map_err(|_| SqlError::Config(format!("要取代的记忆不存在：{id}")))?;
            tracing::info!(id = %old.id, "记忆原地取代：模型显式指定被取代项");
            Some(old)
        }
        None => {
            let nearest = db_memory::search_similar(db, &vector, 1)
                .unwrap_or_default()
                .into_iter()
                .next();
            nearest.and_then(|(old, distance)| {
                (similarity(distance) >= cfg.memory_dedup_min_similarity).then(|| {
                    tracing::debug!(id = %old.id, similarity = similarity(distance), "记忆去重：改写现有条目");
                    old
                })
            })
        }
    };

    let expires_at = resolve_lifetime(kind, content);

    if let Some(old) = target {
        db_memory::revise(db, &old.id, content, kind, expires_at.as_deref(), &vector)?;
        let memory = db_memory::get(db, &old.id)?;
        return Ok(WriteOutcome {
            memory: MemoryRecord::from(&memory),
            kind: WriteKind::Revised,
        });
    }

    let memory = Memory::new(
        content.to_string(),
        kind,
        message_id.map(String::from),
        expires_at,
    );

    if vector.is_empty() {
        db_memory::create_record(db, &memory)?;
    } else {
        db_memory::create(db, &memory, &vector)?;
    }
    Ok(WriteOutcome {
        memory: MemoryRecord::from(&memory),
        kind: WriteKind::Created,
    })
}

/// 到期时间：**强时间锚优先，其次种类的默认窗口**。
///
/// 为什么这两件事要分开决定：
/// - **种类由模型选**，因为"这是性格还是作息"需要读懂内容，代码判断不了；
/// - **寿命由代码算**，因为期限是机械规则。让模型自己填期限，等于让它凭空给一个
///   天数，而那个数字既没法审也没法测——本仓库上一版就是因为这样留下过一组
///   "配了但没人能影响"的 TTL 旋钮。
///
/// 锚点赢过默认窗口：模型写"下周要体检"时种类可能是 trait，但到期日必须按锚点算，
/// 否则它会带着 trait 的永不过期活很久。
fn resolve_lifetime(kind: MemoryKind, content: &str) -> Option<String> {
    if let Some(deadline) = temporal::temporal_anchor(content) {
        return Some(deadline.to_rfc3339());
    }
    kind.default_lifetime_days()
        .map(|days| (Utc::now() + chrono::Duration::days(days)).to_rfc3339())
}

/// 语义检索记忆。**只返回未过期且余弦相似度 ≥ `memory_recall_min_similarity` 的条目**；
/// 向量签名不匹配时返回空结果（检索暂停，等待确认重建）。
pub fn search(db: &Database, query: &str, limit: usize) -> Result<Vec<(Memory, f32)>> {
    // 热路径每轮都走：只查维度与度量（sqlite_master 里 vec0 的建表语句），
    // 不要退回 vec_status()——那会连带扫两次 COUNT(*)。
    if !db.vec_search_usable()? {
        return Ok(Vec::new());
    }
    let vector = embedding::embed_text(query)?;
    let min_similarity = Config::get().memory_recall_min_similarity;
    Ok(db_memory::search_similar(db, &vector, limit)?
        .into_iter()
        .filter(|(_, distance)| similarity(*distance) >= min_similarity)
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

#[cfg(test)]
#[path = "memory_tests.rs"]
mod memory_tests;
