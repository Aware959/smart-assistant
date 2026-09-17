//! 服务层对外视图（可序列化记录）类型，以及 db 模型到视图的转换。

use serde::{Deserialize, Serialize};

use crate::core::types::{MemoryRecord, MessageTurn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

pub(crate) fn memory_to_record(m: &crate::db::memory::Memory) -> MemoryRecord {
    m.into()
}

pub(crate) fn session_to_record(s: crate::db::session::Session) -> SessionRecord {
    SessionRecord {
        id: s.id,
        title: s.title,
        created_at: s.created_at,
        updated_at: s.updated_at,
    }
}

pub(crate) fn message_to_record(m: crate::db::message::Message) -> MessageRecord {
    MessageRecord {
        id: m.id,
        session_id: m.session_id,
        role: m.role,
        content: m.content,
        created_at: m.created_at,
    }
}

pub(crate) fn turn_from(m: crate::db::message::Message) -> MessageTurn {
    MessageTurn {
        role: m.role,
        content: m.content,
        created_at: m.created_at,
    }
}
