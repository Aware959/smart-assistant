//! 外部渠道接入：把第三方平台的消息翻译为内部对话流水线调用。
//!
//! 每个渠道只做三件事：收消息 → 调主线聊天服务（[`crate::core::agent::Agent::chat_stream`]）→ 回消息。
//! 会话隔离由会话服务（[`crate::core::session::Sessions`]）的 `resolve` / `reset` 负责。
//!
//! 主动推送：各渠道维护自己的可推状态，主动引擎
//! 经 [`ChannelsPusher`]（主动外发的具体实现）只读查询后派发文本。

// 未启用任何通道特性时（如只编 sa-web），本模块整体处于惰性状态：
// 保留占位实现以维持 core::intent 的依赖成立，但无需再为死代码报警。
#![cfg_attr(
    not(any(feature = "telegram", feature = "ilink")),
    allow(dead_code, unused_imports, unused_variables)
)]

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "ilink")]
pub mod ilink;

mod push;
mod retry;
mod text;

pub use push::ChannelsPusher;
pub(crate) use retry::{status_retryable, with_send_retry, SendErr, SEND_RETRY};
pub use text::{chunk_text, chunk_text_utf16};

/// 单条入站消息的 AI 处理看门狗上限（秒）：超过即放弃等待，轮询照常继续。
///
/// 阻塞线程池里的 LLM/embedding 调用即便有客户端超时，等待侧仍可能被一次停滞拖住，
/// 这里的 deadline 保证通道不会被一轮对话永久占住。**超时之后各端按各自的方式继续**：
/// Telegram 每轮跑在独立任务里（见 `telegram::spawn_turn`），超时即放开会话名额并把这条
/// 入队静默重试；iLink 仍是串行等待，超时发兜底话术继续轮询。
pub(crate) const AI_DEADLINE_SECS: u64 = 240;

/// 多条消息之间的投递间隔：模拟真人一句接一句敲字的节奏。
///
/// 模型在**一次响应里**就通过并行 `send_message` 决定好说几句，所以这里等的是
/// "发第一条到发第二条"的时间，而不是等模型想。取值按条数递增：连发太多句真人
/// 不像真人，第一句和第二句之间反而最短。
pub(crate) fn pacing_delay(index: usize) -> std::time::Duration {
    match index {
        0 => std::time::Duration::ZERO,
        1 => std::time::Duration::from_millis(900),
        2 => std::time::Duration::from_millis(1400),
        _ => std::time::Duration::from_millis(1900),
    }
}

/// 把通道报文里的消息时间戳（Unix 秒或毫秒）归一化为 RFC3339 世界时字符串。
/// Telegram `Message.date` 为秒级；iLink 类微信报文常为毫秒级。None/非法值返回 None。
pub(crate) fn ts_to_rfc3339(ts: Option<i64>) -> Option<String> {
    let ts = ts?;
    let secs = if ts > 1_000_000_000_000 {
        ts / 1000
    } else {
        ts
    };
    chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339())
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_short_text_untouched() {
        assert_eq!(chunk_text("hello", 4096), vec!["hello".to_string()]);
        assert!(chunk_text("", 10).is_empty());
    }

    #[test]
    fn chunks_long_text_within_limit() {
        let text = "a".repeat(9000);
        let chunks = chunk_text(&text, 4096);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunks_prefer_newline_and_keep_cjk() {
        // 换行在首段范围内 → 首段应在换行处断开
        let text = format!("{}\n{}", "a".repeat(4000), "b".repeat(4000));
        let chunks = chunk_text(&text, 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), text);
        assert!(chunks[0].ends_with('\n'));

        // 中文按字符计数，不断开字符
        let cjk = "你好".repeat(3000);
        let chunks = chunk_text(&cjk, 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), cjk);
    }
}
