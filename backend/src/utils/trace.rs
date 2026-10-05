//! 链路追踪辅助：给一次对话 / 决策打上 `trace_id`，并按开关决定内容详略。
//!
//! 约定：追踪事件统一用 `target: "trace"`，便于日志层单独过滤并落 JSON 文件
//! （server 的 `SMART_ASSISTANT_TRACE_FILE`）。内容默认只记摘要，设
//! `TRACE_CONTENT=1` 才记全文——避免默认把记忆 / 提示词 / 回复全文落盘。

use std::fmt;

use crate::config::Config;

/// 新建一轮对话的追踪 span：自动生成 `trace_id`；`session_id` / `user_message_id`
/// 在流水线中确定后再 `span.record(..)` 补上。
pub fn turn_span() -> tracing::Span {
    tracing::info_span!(
        "chat_turn",
        trace_id = %uuid::Uuid::new_v4(),
        session_id = tracing::field::Empty,
        user_message_id = tracing::field::Empty,
    )
}

/// 生成一个 `trace_id`（异步路径不便跨 `await` 持有 span 时，直接用字段带上）。
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 是否记录内容全文（`TRACE_CONTENT=1`）。
pub fn content_enabled() -> bool {
    Config::get().trace_content
}

/// 单段文本：开启全文时原样输出，否则输出 `<N chars>` 摘要。
pub struct Text<'a>(pub &'a str);

impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if content_enabled() {
            f.write_str(self.0)
        } else {
            write!(f, "<{} chars>", self.0.chars().count())
        }
    }
}

/// 懒渲染文本（集合拼接等昂贵内容）：仅在开启全文时才调用闭包，否则输出 `<omitted>`。
pub struct Lazy<F>(pub F);

impl<F: Fn() -> String> fmt::Display for Lazy<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if content_enabled() {
            f.write_str(&(self.0)())
        } else {
            f.write_str("<omitted>")
        }
    }
}
