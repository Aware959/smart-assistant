//! 编排内核共享的输入输出与可移植值类型。
//!
//! 这些类型不感知任何能力实现 / 存储布局：既能被内核流水线使用，也能被 FFI / HTTP /
//! 通道层直接序列化。服务层与入口层通过 `crate::core::types` 复用同一份定义。

use serde::{Deserialize, Serialize};

/// 一条发给 LLM 的对话消息（角色为 system / user / assistant）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 对话输入：处理层拿到的最低输入契约（会话 / 消息 / 可选的显式历史）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInput {
    pub message: String,
    /// 会话 id。留空时后端自动新建会话并返回新 id。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 显式携带的历史轮次（可选，补充上下文）；留空时后端自动从会话消息构建。
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// 自动构建上下文的方式：缺省按"今天"取消息（日界=用户当地 06:00、带时间戳）；
    /// 显式指定 N 时改为取最近 N 条纯文本。
    #[serde(default)]
    pub history_count: Option<u32>,
    /// 用户端消息的真实发出时刻（RFC3339 世界时，来自通道报文时间戳）。
    /// 缺省时为后端接收时刻；用于时间世界模型的精确时间线。
    #[serde(default)]
    pub user_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatOutput {
    pub session_id: String,
    pub reply: String,
    /// 本条用户消息的 id：宿主在回复交付后调用 `settle_memory` 时用它关联记忆来源。
    pub user_message_id: String,
    /// 本轮沉淀出的记忆。
    ///
    /// `chat_stream` 返回时**恒为 None**——记忆沉淀已后置到回复交付之后，由宿主调用
    /// `settle_memory` 拿到结果后自行填充（响应结构因此保持不变）。
    pub memory: Option<MemoryRecord>,
}

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

/// 记忆视图记录（FFI / HTTP 可序列化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    pub tier: String,
    pub created_at: String,
    pub score: f32,
}

/// 会话持久化视图（内核不感知存储实现）。
#[derive(Debug, Clone)]
pub struct ConversationSession {
    pub id: String,
    pub title: String,
}

/// 一条已入库的消息（内核只关心 id 与时间戳这类标量，不感知行结构）。
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: String,
}

/// 历史消息轮次（供时间线构建 / 提示词渲染，纯值类型）。
#[derive(Debug, Clone)]
pub struct MessageTurn {
    pub role: String,
    pub content: String,
    /// RFC3339 时刻。
    pub created_at: String,
}