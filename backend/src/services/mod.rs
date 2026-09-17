//! 服务层：在 `db::*` 之上提供会话 / 消息 / 记忆的管理能力，
//! 并产出 FFI / HTTP 可序列化的视图记录类型。
//!
//! - [`Conversation`] 适配器实现端口 `ConversationStore`，供内核注入；
//! - 其余能力以文件级门面函数暴露（`list_sessions` / `search_memory` 等），
//!   供宿主（lib / api / bin）直接调用；
//! - 视图记录类型在 [`records`]，db 模型与视图的转换也在该模块内完成。

mod conversation;
mod memory;
mod message;
mod records;
mod session;

pub use conversation::Conversation;
pub use memory::{add_memory, delete_memory, list_memories, search_memory};
pub use message::list_messages;
pub use records::{MessageRecord, SessionRecord};
pub use session::{create_session, delete_session, list_sessions};