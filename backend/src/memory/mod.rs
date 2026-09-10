//! 记忆功能：LLM 记忆判定（extraction）+ 向量存取（store）。
//!
//! 该模块是记忆能力的唯一入口：判定要不要记、记住什么（extraction），
//! 以及记到哪里、怎么查（store）。不涉及会话/消息/图谱。

pub mod extraction;
pub mod store;
