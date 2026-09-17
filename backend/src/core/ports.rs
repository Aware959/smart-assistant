//! 编排内核的能力端口（ports）：内核只依赖这些契约，不感知任何能力实现与存储布局。
//!
//! 能力层（llm / embedding / memory / world / services）各自实现这些 trait，
//! 通过 `Agent::with_ports` 注入内核。存储句柄由宿主层持有，以 `Arc` 共享给各适配器，
//! 因此端口方法签名里不出现任何 `db` 相关类型。

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::core::types::{
    ChatMessage, ConversationSession, Extraction, MemoryHit, MemoryRecord, MessageTurn, StoredMessage,
};
use crate::error::Result;

/// LLM 接入端口：对话 / 结构化输出 / 流式回复。
pub trait ChatLlm: Send + Sync {
    /// 非流式对话，返回回复文本。
    fn complete(&self, messages: &[ChatMessage]) -> Result<String>;

    /// 强制结构化输出（`json_schema` 或 `json_object`，由配置决定）。
    /// `schema` 为 JSON Schema；`name` 为该 schema 的标识名。
    fn complete_structured(
        &self,
        messages: &[ChatMessage],
        model: &str,
        name: &str,
        schema: Value,
    ) -> Result<String>;

    /// 流式对话：每个文本增量在生成时同步回调 `on_delta`，返回完整文本。
    fn complete_stream(
        &self,
        messages: &[ChatMessage],
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String>;
}

/// 向量化端口：把文本映射为稠密向量。
pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// 对话持久化端口：会话与消息（对话本身是内核领域数据，但落盘方式可替换）。
pub trait ConversationStore: Send + Sync {
    /// 复用传入的会话 id；不存在或未传则自动新建。
    fn get_or_create_session(&self, id: Option<&str>) -> Result<ConversationSession>;

    /// 首条消息自动生成会话标题。
    fn set_session_title(&self, session_id: &str, title: &str) -> Result<()>;

    /// 会话活跃时间戳刷新。
    fn touch_session(&self, session_id: &str) -> Result<()>;

    /// 追加一条消息（时间取存储侧"现在"）。
    fn add_message(&self, session_id: &str, role: &str, content: &str) -> Result<StoredMessage>;

    /// 追加一条消息并指定其真实发出时刻（通道报文时间戳）。
    fn add_message_at(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        at: DateTime<Utc>,
    ) -> Result<StoredMessage>;

    /// 取某个时点之后的全部消息（时间正序）。
    fn messages_since(
        &self,
        session_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<MessageTurn>>;

    /// 取最近 N 条消息（时间正序）。
    fn recent_messages(&self, session_id: &str, limit: usize) -> Result<Vec<MessageTurn>>;
}

/// 记忆端口：判定 / 检索 / 沉淀。
pub trait MemoryStore: Send + Sync {
    /// 判定"是否值得沉淀记忆"，并产出记忆事实与关系事件标签。
    /// 实现内部按两步调用（先出标签，判定值得记才做事实化），端口不暴露这个细节。
    fn extract(&self, text: &str) -> Result<Extraction>;

    /// 语义检索记忆（已过滤相关度、剔除过期条目）。
    fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>>;

    /// 沉淀一段记忆为事实（含去重 / 分层），返回可序列化的记忆记录。
    fn store(
        &self,
        content: &str,
        memory_type: &str,
        tier: &str,
        message_id: Option<&str>,
    ) -> Result<MemoryRecord>;
}

/// 世界状态端口：AI 的"进行时"感知 + 关系演化的能力入口。
pub trait WorldState: Send + Sync {
    /// 用本条入站消息的真实时刻增量重算对方作息画像（无映射时静默跳过）。
    fn observe(&self, session_id: &str) -> Result<()>;

    /// 按消息的关系事件标签调整两人关系（未知标签回退 neutral）。
    fn apply_event(&self, session_id: &str, relation: &str) -> Result<()>;

    /// 组装并渲染"此刻的世界"片段（供提示词注入）。
    fn snapshot_text(&self, session_id: &str) -> String;

    /// 推断的对方时区偏移（分钟，东为正；无画像时用东八区兜底）。
    fn user_offset_minutes(&self, session_id: &str) -> i64;
}