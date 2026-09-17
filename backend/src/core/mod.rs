//! 编排内核（core）：AI 的核心编排能力，可复用的单轮对话流水线。
//!
//! 该层定义能力契约（[`ports`]）与输入输出类型（[`types`]），
//! 并在 [`agent`] 中面向契约编程，不感知任何能力实现。
//! LLM 接入 / 向量化 / 记忆 / 世界状态都是实现这些契约的能力层。

pub mod agent;
pub mod ports;
pub mod prompt;
pub mod types;