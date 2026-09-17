//! Smart Assistant 应用外壳 crate 根。
//!
//! - [`app::Assistant`]：组合根（持有 `Arc<Mutex<Database>>` + 编排内核），
//!   实现宿主窄端口 [`crate::host::Host`] 供组件注入；
//! - [`host`]：宿主窄端口（object-safe trait）——组件层只依赖这张"能力契约"；
//! - [`ffi`]：UniFFI 导出（Android 绑定）；
//! - 其余模块：`config` 环境配置、`core` 编排内核、`db` SQLite 层、`services`
//!   服务门面、`memory` 记忆业务、`world` 世界引擎、`proactive` 主动陪伴、
//!   `channels` 渠道、`llm` 模型调用、`api` HTTP 接口。
//!
//! ## 依赖方向（硬规则，见 `AGENTS.md`「分层与依赖」）
//!
//! ```text
//!           组合根 Assistant ----实现----> host::Host (窄端口 trait)
//!                  ▲                              ▲
//!                  │ 依赖                        │ 依赖
//!              api / ffi / bin              channels / proactive / world
//! ```
//!
//! 组件层（`channels` / `proactive` / `world`）**一律只认 `Arc<dyn Host>`**，
//! 不 import `crate::app::Assistant`；`core` 为纯净内核，依赖方向单向
//! （`core ⊥ db / llm / services / memory`）。分层校验见 [`scripts::check_layers`]。

pub mod app;
pub mod channels;
pub mod config;
pub mod core;
pub mod db;
pub mod error;
pub mod ffi;
pub mod host;
pub mod llm;
pub mod memory;
pub mod proactive;
pub mod services;
pub mod world;

#[cfg(feature = "desktop")]
pub mod api;

pub use app::Assistant;
pub use services::{MessageRecord, SessionRecord};

pub use crate::core::types::{ChatInput, ChatOutput, ChatTurn, MemoryHit, MemoryRecord};

pub use crate::core::types::{
    ChatMessage as CoreChatMessage, ChatTurn as CoreChatTurn, MemoryRecord as CoreMemoryRecord,
};

// UniFFI 脚手架：必须在 crate 根展开（定义 crate::UniFfiTag），
// 具体导出见 [`ffi`]。
uniffi::setup_scaffolding!();
