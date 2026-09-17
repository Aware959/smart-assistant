//! 服务层会话能力：新建 / 列出 / 删除。

use crate::db::Database;
use crate::error::Result;

use super::records::{session_to_record, SessionRecord};

/// 新建会话，返回会话 id。
pub fn create_session(db: &Database, title: &str) -> Result<String> {
    let session = crate::db::session::create(db, title)?;
    Ok(session.id)
}

/// 列出会话（按最近更新倒序）。
pub fn list_sessions(db: &Database, limit: u32) -> Result<Vec<SessionRecord>> {
    Ok(crate::db::session::list(db, limit as usize)?
        .into_iter()
        .map(session_to_record)
        .collect())
}

/// 删除会话及其全部消息（记忆保留，来源 message_id 置空）。
pub fn delete_session(db: &Database, id: &str) -> Result<()> {
    crate::db::session::delete(db, id)
}
