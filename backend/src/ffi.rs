//! UniFFI 导出：把 [`crate::Assistant`] 的能力暴露给 Android(Kotlin) 等宿主。
//!
//! 脚手架 `uniffi::setup_scaffolding!()` 在 crate 根展开（见 `lib.rs`）。

#[uniffi::export]
impl Assistant {
    #[uniffi::constructor]
    pub fn new_ffi(db_path: String) -> FfiResult<Self> {
        Self::new(&db_path).map_err(Into::into)
    }

    #[uniffi::constructor]
    pub fn new_in_memory_ffi() -> FfiResult<Self> {
        Self::new_in_memory().map_err(Into::into)
    }

    #[uniffi::method]
    pub fn chat_ffi(&self, message: String, session_id: Option<String>, history_json: String) -> FfiResult<String> {
        let history: Vec<ChatTurn> = serde_json::from_str(&history_json)?;
        let input = ChatInput {
            message,
            session_id,
            history,
            history_count: None,
            user_time: None,
        };
        let mut output = self.chat_stream(&input, |_| {})?;
        // 端侧同步调用：回复拿到后再补记忆沉淀（失败降级为 None，不影响已得到的回复）。
        output.memory =
            self.settle_memory(&output.session_id, &output.user_message_id, &input.message);
        serde_json::to_string(&output).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn create_session_ffi(&self, title: String) -> FfiResult<String> {
        self.create_session(&title).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn list_sessions_ffi(&self, limit: u32) -> FfiResult<String> {
        let sessions = self.list_sessions(limit)?;
        serde_json::to_string(&sessions).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn list_messages_ffi(&self, session_id: String, limit: u32) -> FfiResult<String> {
        let messages = self.list_messages(&session_id, limit)?;
        serde_json::to_string(&messages).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn delete_session_ffi(&self, id: String) -> FfiResult<()> {
        self.delete_session(&id).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn search_memory_ffi(&self, query: String, limit: u32) -> FfiResult<String> {
        let hits = self.search_memory(&query, limit)?;
        serde_json::to_string(&hits).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn list_memories_ffi(&self, limit: u32) -> FfiResult<String> {
        let memories = self.list_memories(limit)?;
        serde_json::to_string(&memories).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn add_memory_ffi(
        &self,
        content: String,
        memory_type: String,
        message_id: Option<String>,
    ) -> FfiResult<String> {
        self.add_memory(&content, &memory_type, message_id.as_deref())
            .map_err(Into::into)
    }

    #[uniffi::method]
    pub fn delete_memory_ffi(&self, id: String) -> FfiResult<()> {
        self.delete_memory(&id).map_err(Into::into)
    }
}

use crate::core::types::ChatTurn;
use crate::error::FfiResult;
use crate::{Assistant, ChatInput};