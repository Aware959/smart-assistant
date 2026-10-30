//! 记忆召回结果的渲染约定。
//!
//! 检索由 [`crate::core::memory::Store::search`] 负责，并已按
//! `memory_recall_min_similarity` 过滤、剔除过期条目。这里只保留模型工具
//! 回填需要的展示约定。

/// 单轮对话中最多召回的相关历史记忆条数。
pub(crate) const RECALL_LIMIT: usize = 5;

/// 渲染成带 id 的多行文本（`recall_memory` 工具回填用）。
///
/// **必须带 id**：`remember` 的 `supersedes` 要指向被取代的那条，模型唯一的
/// 拿到 id 的途径就是召回结果。id 用完整 uuid 而非短码——省下的那几个字符
/// 不值得换来"撞 id 就改错记忆"的风险。
pub(crate) fn id_lines(memory_hits: &[crate::core::memory::MemoryHit]) -> String {
    memory_hits
        .iter()
        .map(|hit| format!("[{}] {}", hit.id, hit.content))
        .collect::<Vec<_>>()
        .join("\n")
}
