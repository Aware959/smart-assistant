//! LLM 对话消息：内核与模型能力之间的值契约。
//!
//! 一个类型同时承担两种角色：组装出的**历史轮次**、发给模型的**请求消息**
//! （此前 `ChatTurn` / `ChatMessage` 两个同形类型已合并到此）。
//! 是纯内核值类型——**不带任何序列化 derive**，需要变成 HTTP / 模型线协议
//! 由对应适配层（`llm` / `api`）自行负责。
//!
//! 注意与 [`crate::core::session::MessageTurn`] 的区别：后者是**已持久化**的
//! 消息（多一个 `created_at`），本类型只表达"内容 + 说话人"。

/// 模型发起的一次工具调用（`genai::chat::ToolCall` 的内核镜像，不依赖 provider 类型）。
#[derive(Debug, Clone)]
pub struct ChatCall {
    /// 本次调用的稳定 id，回填结果时用于配对。
    pub call_id: String,
    /// 工具名。
    pub fn_name: String,
    /// 已解析的参数对象。
    pub fn_arguments: serde_json::Value,
}

impl ChatCall {
    pub fn new(
        call_id: impl Into<String>,
        fn_name: impl Into<String>,
        fn_arguments: serde_json::Value,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            fn_name: fn_name.into(),
            fn_arguments,
        }
    }

    /// 取字符串参数，缺失或类型不符时返回 `None`。
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.fn_arguments.get(key)?.as_str()
    }
}

/// 一条对话消息（角色为 system / user / assistant / tool）。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// assistant 轮发出的工具调用；`role == "assistant"` 且非空时表示"模型在等结果"。
    pub tool_calls: Vec<ChatCall>,
    /// `role == "tool"` 时回应的调用 id。
    pub tool_call_id: Option<String>,
    /// `role == "tool"` 时的工具名，部分 provider 要求回填。
    pub tool_name: Option<String>,
}

impl ChatMessage {
    /// 纯文本消息（system / user / assistant）。
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }

    /// assistant 轮：携带出站工具调用。`content` 通常为空（正文走 `send_message` 工具）。
    pub fn assistant_calls(content: impl Into<String>, tool_calls: Vec<ChatCall>) -> Self {
        Self {
            tool_calls,
            ..Self::new("assistant", content)
        }
    }

    /// tool 轮：回填某个调用的结果。
    pub fn tool_result(call: &ChatCall, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call.call_id.clone()),
            tool_name: Some(call.fn_name.clone()),
            ..Self::new("tool", content)
        }
    }
}

#[cfg(test)]
#[path = "message_tests.rs"]
mod message_tests;
