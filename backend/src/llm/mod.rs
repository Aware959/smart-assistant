//! 模型调用层：LLM 对话与文本向量化（embedding）统一走 genai 阻塞 API。
//!
//! - [`chat`]：对话（非流式 / 结构化 / 流式）；
//! - [`embedding`]：文本向量化；
//! - [`genai_client`]：共享 Client 构造与全局 runtime 的同步封装。

pub mod chat;
pub mod embedding;
pub mod genai_client;
