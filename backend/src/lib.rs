//! Smart Assistant 应用外壳 crate 根。
//!
//! - [`core`]：**应用本体**——判断、编排与领域逻辑都收在这里（对话流水线 /
//!   会话 / 记忆 / 意图判定 / 意图状态追踪 / [`core::app::App`] 组装层）；
//! - 留在 core 之外的**外部适配层**：`db` SQLite（纯 SQL，表所有权归 core 内
//!   对应模块）、`llm` 模型调用（对话 / 向量化）、`channels` / `api` 传输层，
//!   `config` 环境配置、`error` 共享错误类型。
//!
//! 依赖方向（硬规则，见 `AGENTS.md`「分层与依赖」）：core 不反向依赖传输层
//! （`channels` / `api`），唯一例外是主动外发的具体能力 [`channels::ChannelsPusher`]；
//! 传输层直接持主线聊天服务（[`core::agent::Agent`]）与必要的服务引用走方法调用，
//! 不碰 db，数据一律经各服务方法读写，db 不出壳。
//!
//! ## 分层守卫
//!
//! `tests/layer_guard.rs` 把上述规则固化成可执行的测试。

pub mod channels;
pub mod config;
pub mod core;
pub mod db;
pub mod error;
pub mod llm;
pub mod utils;

#[cfg(feature = "desktop")]
pub mod api;

pub use crate::core::agent::{Agent, ChatInput, ChatOutput};
pub use crate::core::app::App;
pub use crate::core::intent::IntentCandidate;
pub use crate::core::memory::{MemoryHit, MemoryRecord};
pub use crate::core::schema::message::ChatMessage;
