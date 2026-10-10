//! 记忆召回 → 上下文片段。
//!
//! 只消费记忆能力的具体实现（[`crate::core::memory::Store`]）；召回已按
//! `memory_recall_min_similarity` 过滤、剔除过期条目，由记忆能力层负责。
//! 这里只做"取"与两种渲染形态的组装。

use crate::core::memory::MemoryHit;
use crate::core::memory::Store;
use crate::error::Result;

/// 单轮对话中最多召回的相关历史记忆条数。
pub(crate) const RECALL_LIMIT: usize = 5;

/// 取与 `query` 语义相关的记忆。
pub(crate) fn fetch(memory: &Store, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
    memory.search(query, limit)
}

/// 渲染成带 id 的多行文本（`recall_memory` 工具回填用）。
///
/// **必须带 id**：`remember` 的 `supersedes` 要指向被取代的那条，模型唯一的
/// 拿到 id 的途径就是召回结果。id 用完整 uuid 而非短码——省下的那几个字符
/// 不值得换来"撞 id 就改错记忆"的风险。
pub(crate) fn id_lines(memory_hits: &[MemoryHit]) -> String {
    memory_hits
        .iter()
        .map(|hit| format!("[{}] {}", hit.id, hit.content))
        .collect::<Vec<_>>()
        .join("\n")
}
