//! 服务层消息能力：列出会话内的消息。

use crate::db::Database;
use crate::error::Result;

use super::records::{message_to_record, MessageRecord};

/// 列出某会话内的消息（时间正序）。
pub fn list_messages(db: &Database, session_id: &str, limit: u32) -> Result<Vec<MessageRecord>> {
    Ok(crate::db::message::list_by_session(db, session_id, limit as usize)?
        .into_iter()
        .map(message_to_record)
        .collect())
}
