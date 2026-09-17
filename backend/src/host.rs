//! 宿主窄端口（依赖倒置的锚点）：组件层不再认识 `Arc<Assistant>` 大对象，
//! 只认这张"能力契约"，由组合根 [`crate::Assistant`] 实现并注入。
//!
//! ## 为什么要有它
//!
//! 组件（`channels` / `proactive` / `world`）需要的能力其实只有五个：
//!
//! - 数据库句柄（读写会话 / 消息 / 记忆 / 世界状态）；
//! - 一轮对话（LLM 流水线，流式）；
//! - 回复后的记忆沉淀（判定 / 关系演化 / 落库）；
//! - 一轮"纯 LLM 决策"（开口 / 延续 / 决策判断）；
//! - 此刻世界的叙事快照文本（开口前认清"世界是什么"）。
//!
//! 它们不需要 `Arc<Assistant>` 上那 36 个 pub 方法的全部——拿大对象等于
//! 让组件偷看宿主内部、把组合根的注入口径变成组件签名的一部分。
//! [`Host`] 把需要的方法收窄成一张可 object-safe 的 trait，组件统一依赖
//! `Arc<dyn Host>`，依赖方向变为 **组件 → 端口（Host）→ 宿主实现**。
//!
//! ## 依赖方向
//!
//! ```text
//!         组件层 (channels / proactive / world)
//!                    │  只 import crate::host::Host
//!                    ▼
//!               Host (object-safe trait)
//!                    ▲
//!                    │  impl Host for Assistant
//!                  组合根 (crate::Assistant)
//! ```
//!
//! 组件不再 import `crate::Assistant` —— 层校验脚本负责把这条做成硬规则。

use std::sync::{Arc, MutexGuard};

use crate::core::ports::ChatLlm;
use crate::core::types::{ChatInput, ChatOutput, MemoryRecord};
use crate::db::Database;
use crate::error::Result;

/// 宿主窄端口。
///
/// 方法全部 object-safe（无泛型、无并发返回借用），可包装成 `Arc<dyn Host>`
/// 分发给各组件。实现方：组合根 [`crate::Assistant`]。
pub trait Host: Send + Sync {
    /// 当前的数据库句柄。调用方须尽快释放锁守卫，勿跨 `.await` 持锁。
    fn db(&self) -> MutexGuard<'_, Database>;

    /// 一轮对话（阻塞、流式）：`on_delta` 每个增量到达时回调一次。
    /// 与 [`crate::Assistant::chat_stream`] 是同一能力；`&mut dyn FnMut` 形式
    /// 是为了满足 object-safe。`+ Send` 是为了能透传给内核的
    /// `Agent::chat_stream`（`F: FnMut + Send`）。
    fn chat_stream(
        &self,
        input: &ChatInput,
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<ChatOutput>;

    /// 回复交付后的记忆沉淀（判定 → 关系演化 → 落库），阻塞调用。
    ///
    /// 与 [`Host::chat_stream`] 拆开是刻意的：判定要调 LLM、沉淀要调 embedding，
    /// 挂在对话路径上既拖首字延迟、失败还会吞掉回复。调用方应在回复送达之后调用本方法
    /// （宜放 `spawn_blocking` 等后台线程）。任何环节失败只记日志并返回 `None`。
    fn settle_memory(
        &self,
        session_id: &str,
        user_message_id: &str,
        text: &str,
    ) -> Option<MemoryRecord>;

    /// 一轮"纯 LLM 决策"：不走会话流水线，只拿模型给的一次完整回答。
    fn chat_llm(&self) -> Arc<dyn ChatLlm>;

    /// 此刻世界的叙事快照文本（AI 开口前认清"世界是什么"）。
    fn world_snapshot_text(&self, session_id: &str) -> String;
}

/// 便捷助手：把宿主实现包装成可按需克隆注入的窄端口。
pub type HostArc = Arc<dyn Host>;
