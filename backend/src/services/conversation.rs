//! 端口适配器：会话 / 消息持久化。

use std::sync::{Arc, Mutex};

use crate::core::ports::ConversationStore;
use crate::core::types::{ConversationSession, MessageTurn, StoredMessage};
use crate::db::Database;
use crate::error::Result;

use super::records::turn_from;

/// 以数据库为后端的 [`ConversationStore`] 实现。
/// 数据库句柄由宿主持有并以 `Arc<Mutex<Database>>` 注入，自身不拥有连接。
#[derive(Clone)]
pub struct Conversation {
    db: Arc<Mutex<Database>>,
}

impl Conversation {
    pub fn new(db: Arc<Mutex<Database>>) -> Self {
        Self { db }
    }

    fn lock_db(&self) -> std::sync::MutexGuard<'_, Database> {
        self.db.lock().expect("db mutex poisoned")
    }
}

impl ConversationStore for Conversation {
    fn get_or_create_session(&self, id: Option<&str>) -> Result<ConversationSession> {
        let db = self.lock_db();
        let existing = id.and_then(|id| crate::db::session::get(&db, id).ok()).flatten();
        let session = match existing {
            Some(s) => s,
            None => crate::db::session::create(&db, "")?,
        };
        Ok(ConversationSession {
            id: session.id,
            title: session.title,
        })
    }

    fn set_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        crate::db::session::update_title(&self.lock_db(), session_id, title)
    }

    fn touch_session(&self, session_id: &str) -> Result<()> {
        crate::db::session::touch(&self.lock_db(), session_id)
    }

    fn add_message(&self, session_id: &str, role: &str, content: &str) -> Result<StoredMessage> {
        let m = crate::db::message::create(&self.lock_db(), session_id, role, content)?;
        Ok(StoredMessage { id: m.id })
    }

    fn add_message_at(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<StoredMessage> {
        let m = crate::db::message::create_at(&self.lock_db(), session_id, role, content, at)?;
        Ok(StoredMessage { id: m.id })
    }

    fn messages_since(
        &self,
        session_id: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<MessageTurn>> {
        Ok(crate::db::message::list_since(&self.lock_db(), session_id, since)?
            .into_iter()
            .map(turn_from)
            .collect())
    }

    fn recent_messages(&self, session_id: &str, limit: usize) -> Result<Vec<MessageTurn>> {
        Ok(crate::db::message::list_recent(&self.lock_db(), session_id, limit)?
            .into_iter()
            .map(turn_from)
            .collect())
    }
}