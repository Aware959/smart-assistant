//! 记忆召回 → 上下文片段。
//!
//! 只消费记忆能力的具体实现（[`crate::core::memory::Store`]）；召回已按
//! `memory_recall_threshold` 过滤、剔除过期条目，由记忆能力层负责。
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

/// 渲染成对话 system prompt 里的"你记得这些事"块。
pub(crate) fn build_recall_context(memory_hits: &[MemoryHit]) -> String {
    let mut ctx = String::new();
    if !memory_hits.is_empty() {
        ctx.push_str("你记得这些事：\n");
        for hit in memory_hits.iter().take(RECALL_LIMIT) {
            ctx.push_str(&format!("- {}\n", hit.content));
        }
    }
    ctx
}

/// 渲染成纯内容多行文本（主动决策阅读用，不带列表前缀）。
pub(crate) fn plain_lines(memory_hits: &[MemoryHit]) -> String {
    memory_hits
        .iter()
        .map(|hit| hit.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}
