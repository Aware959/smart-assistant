//! 应用本体（core）：整个智能体的判断、编排与领域逻辑都收在这里。
//!
//! 分界：只把**外部适配层**留在 core 之外——`db`（SQLite）、`llm`（模型调用）、
//! `channels` / `api`（传输层）。core 内部：
//!
//! - [`agent`]：单轮对话流水线（编排 + 记忆写入工具）；
//! - [`context`]：上下文组装（历史时间线 / 记忆召回 / 提示词拼装的取数与渲染）；
//! - [`session`]：会话内存运行时（`Sessions`，首碰水合 + 写穿）；
//! - [`memory`]：记忆（模型主动写入/存储/召回 + 关系/作息纯实现）；
//! - [`sticker`]：贴纸（表情包）目录——选择键 → `file_id` 的缓存映射；
//! - [`self_profile`]：AI 自我属性（性格/人格/三观/爱好，人设数据源）；
//! - [`intent`]：意图判定引擎（是否开口：主动发起 / 回复后延续）；
//! - [`schema`]：跨领域共享的值类型（无法归到单一领域），如 `message`
//!   ——LLM 对话消息值契约（内核与模型能力的契约）；
//! - [`app`]：组装层 `App`（纯装配，装配后组件直接持服务引用）；
//! - 外部能力按具体类型直接接线：LLM = [`crate::llm::chat::Llm`]，
//!   记忆 = [`crate::core::memory::Store`]，主动推送 = [`crate::channels::ChannelsPusher`]。

pub mod agent;
pub mod context;
pub mod intent;
pub mod memory;
pub mod schema;
pub mod self_profile;
pub mod session;
pub mod sticker;

pub mod app;
