//! LLM 对话消息：内核与模型能力之间的值契约。
//!
//! 一个类型同时承担两种角色：组装出的**历史轮次**、发给模型的**请求消息**
//! （此前 `ChatTurn` / `ChatMessage` 两个同形类型已合并到此）。
//! 是纯内核值类型——**不带任何序列化 derive**，需要变成 HTTP / 模型线协议
//! 由对应适配层（`llm` / `api`）自行负责。
//!
//! 注意与 [`crate::core::session::MessageTurn`] 的区别：后者是**已持久化**的
//! 消息（多一个 `created_at`），本类型只表达"内容 + 说话人"。

/// 一条对话消息（角色为 system / user / assistant）。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}
