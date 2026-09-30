//! 会话域（session）：会话 / 消息 / 渠道映射的唯一读写方。
//!
//! [`Sessions`] 以内存驻留（首碰 / 启动水合 + 变化即写穿）持有会话状态，是
//! `channel_sessions / sessions / messages` 三张表的唯一写入方。本模块同时定义
//! 会话域的值类型（[`ConversationSession`] / [`StoredMessage`] / [`MessageTurn`]）：
//! 它们只在会话与消费方（上下文组装等）之间流转，不属于跨传输边界契约，
//! 因此跟随所属域，定义在本模块。

mod sessions;

pub use sessions::{
    classify_inbound, InboundFreshness, Sessions, FRESH_WINDOW_MINUTES, MAX_REPLYABLE_HOURS,
};

/// 会话视图（不感知存储实现）。
#[derive(Debug, Clone)]
pub struct ConversationSession {
    pub id: String,
    pub title: String,
}

/// 一条已入库的消息（只关心 id 这类标量，不感知行结构）。
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
