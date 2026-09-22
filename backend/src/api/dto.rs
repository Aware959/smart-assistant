//! HTTP 线协议类型（请求 / 响应 DTO）。
//!
//! 只服务 `api` 传输层：进入内核前转成内核类型，离开内核后由内核类型转出。
//! 与内核内部类型**刻意分开定义**（即使形状相同），序列化关注点不外泄到 core——
//! core 的值类型一律不带 serde derive。

use serde::{Deserialize, Serialize};

use crate::core::agent::{ChatInput, ChatOutput};
use crate::core::memory::MemoryRecord;
use crate::core::schema::message::ChatMessage;

/// 一条显式历史轮次（HTTP 入参）。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatTurnDto {
    pub role: String,
    pub content: String,
}

/// 对话请求体（POST /chat/stream 与 WS 共用）。
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    /// 会话 id；留空后端自动新建。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 显式携带的历史轮次（可选）；缺省用后端自动构建的会话上下文。
    #[serde(default)]
    pub history: Vec<ChatTurnDto>,
    /// 自动构建上下文用最近几条历史消息；缺省 6。
    #[serde(default)]
    pub history_count: Option<u32>,
}

impl From<ChatRequest> for ChatInput {
    fn from(r: ChatRequest) -> Self {
        ChatInput {
            message: r.message,
            session_id: r.session_id,
            history: r
                .history
                .into_iter()
                .map(|t| ChatMessage {
                    role: t.role,
                    content: t.content,
                })
                .collect(),
            history_count: r.history_count,
            user_time: None,
        }
    }
}

/// 记忆视图（HTTP 出参）。
#[derive(Debug, Serialize)]
pub struct MemoryRecordDto {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    pub tier: String,
    pub expires_at: Option<String>,
    pub message_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<MemoryRecord> for MemoryRecordDto {
    fn from(m: MemoryRecord) -> Self {
        Self {
            id: m.id,
            content: m.content,
            memory_type: m.memory_type,
            tier: m.tier,
            expires_at: m.expires_at,
            message_id: m.message_id,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// 对话完成事件（SSE `event: done` / WS 结束帧）。
#[derive(Debug, Serialize)]
pub struct ChatOutputDto {
    pub session_id: String,
    pub reply: String,
    pub user_message_id: String,
    pub memory: Option<MemoryRecordDto>,
}

impl From<ChatOutput> for ChatOutputDto {
    fn from(out: ChatOutput) -> Self {
        Self {
            session_id: out.session_id,
            reply: out.reply,
            user_message_id: out.user_message_id,
            memory: out.memory.map(MemoryRecordDto::from),
        }
    }
}
