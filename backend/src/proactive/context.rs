//! 上下文组装：为主动决策提供"最近聊了什么 + 关于对方记得的事"。

use crate::db::Database;

/// 最近 `n` 条历史（时间正序），格式化为便于 LLM 阅读的多行文本。
pub fn recent_history(db: &Database, session_id: &str, n: usize) -> String {
    match crate::db::message::list_recent(db, session_id, n) {
        Ok(msgs) => msgs
            .into_iter()
            .map(|m| format!("{}: {}", role_label(&m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => String::new(),
    }
}

/// 与 `query` 语义相关的记忆召回（文本）。
///
/// 召回已按 `memory_recall_threshold` 过滤、过期记忆剔除（见 [`crate::memory::store::search`]）；
/// 向量表不可用（如需要重建）或检索失败时静默降级为空，主动推送不应因记忆问题失败。
pub fn recall(db: &Database, query: &str) -> String {
    let Ok(hits) = crate::memory::store::search(db, query, 3) else {
        return String::new();
    };
    let mut lines = Vec::new();
    for (memory, _) in hits {
        lines.push(memory.content);
    }
    lines.join("\n")
}

/// 角色标签：与按天时间线格式保持一致（用户/AI），避免模型模仿"对方/你"前缀
fn role_label(role: &str) -> &str {
    match role {
        "user" => "用户",
        "assistant" => "AI",
        _ => role,
    }
}