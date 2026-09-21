//! 应用本体（core）：整个智能体的判断、编排与领域逻辑都收在这里。
//!
//! 分界：只把**外部适配层**留在 core 之外——`db`（SQLite）、`llm`（模型调用）、
//! `channels` / `api`（传输层）。core 内部：
//!
//! - [`agent`]：单轮对话流水线（编排 + 记忆沉淀后置）；
//! - [`context`]：上下文组装（历史时间线 / 记忆召回 / 提示词拼装的取数与渲染）；
//! - [`session`]：会话内存运行时（`Sessions`，首碰水合 + 写穿）；
//! - [`memory`]：记忆（用户事实提取/存储/召回 + 关系/作息纯实现）；
//! - [`intent`]：意图判定引擎（是否开口：主动发起 / 回复后延续）；
//! - [`message`]：LLM 对话消息值类型（内核与模型能力的契约）；
//! - [`app`]：组装层 `App`（纯装配，装配后组件直接持服务引用）；
//! - 外部能力按具体类型直接接线：LLM = [`crate::llm::chat::Llm`]，
//!   记忆 = [`crate::core::memory::Store`]，主动推送 = [`crate::channels::ChannelsPusher`]。

pub mod agent;
pub mod context;
pub mod intent;
pub mod memory;
pub mod message;
pub mod session;

pub mod app;