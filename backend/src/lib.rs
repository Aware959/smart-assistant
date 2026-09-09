pub mod agent;
pub mod config;
pub mod db;
pub mod embedding;
pub mod error;
pub mod extractor;
pub mod genai_client;
pub mod llm;
pub mod memory;
pub mod services;

#[cfg(feature = "desktop")]
pub mod api;

pub use services::{
    EntityRecord, MemoryHit, MemoryRecord, MessageRecord, RelationRecord, SessionRecord,
};

use serde::{Deserialize, Serialize};

use crate::error::{FfiResult, Result};

// UniFFI 需要这些类型可序列化往返，这里复用到 extractor 中的类型定义。
use crate::extractor::parser::{ExtractedEntity, ExtractedRelation};

/// 应用外壳：持有编排层 [`agent::Agent`]，对外提供 UniFFI 导出与能力门面。
/// 业务流水线在 Agent 中实现，本层不做状态与逻辑，仅做薄委托与类型映射。
#[derive(uniffi::Object)]
pub struct Assistant {
    inner: agent::Agent,
}

impl Assistant {
    /// 打开或创建数据库，并初始化 schema。
    pub fn new(db_path: &str) -> Result<Self> {
        Ok(Self {
            inner: agent::Agent::new(db_path)?,
        })
    }

    pub fn new_in_memory() -> Result<Self> {
        Ok(Self {
            inner: agent::Agent::new_in_memory()?,
        })
    }

    /// 对话主入口（流式）：完整流水线见 [`agent::Agent::chat_stream`]。
    ///
    /// 消息是记录、记忆是事实，二者分离；历史上下文自动取该会话最近 `history_count`
    /// 条消息（缺省 6），`on_delta` 在生成线程上同步回调文本片段。
    pub fn chat_stream<F>(&self, input: &ChatInput, on_delta: F) -> Result<ChatOutput>
    where
        F: FnMut(&str) + Send,
    {
        self.inner.chat_stream(input, on_delta)
    }

    /// 只提取不对话：直接对文本做实体关系提取并落库。
    pub fn extract_and_store(&self, content: &str, memory_type: &str) -> Result<ExtractionOutput> {
        self.inner.extract_and_store(content, memory_type)
    }

    // ---------- 服务门面（委托 services 层） ----------

    /// 语义检索记忆。
    pub fn search_memory(&self, query: &str, limit: u32) -> Result<Vec<MemoryHit>> {
        let db = self.inner.lock_db();
        services::search_memory(&db, query, limit)
    }

    /// 手动添加一条记忆，返回记忆 id。
    pub fn add_memory(
        &self,
        content: &str,
        memory_type: &str,
        message_id: Option<&str>,
    ) -> Result<String> {
        let db = self.inner.lock_db();
        services::add_memory(&db, content, memory_type, message_id)
    }

    /// 列出全部记忆。
    pub fn list_memories(&self, limit: u32) -> Result<Vec<MemoryRecord>> {
        let db = self.inner.lock_db();
        services::list_memories(&db, limit)
    }

    /// 新建会话，返回会话 id。
    pub fn create_session(&self, title: &str) -> Result<String> {
        let db = self.inner.lock_db();
        services::create_session(&db, title)
    }

    /// 列出会话（按最近更新倒序）。
    pub fn list_sessions(&self, limit: u32) -> Result<Vec<SessionRecord>> {
        let db = self.inner.lock_db();
        services::list_sessions(&db, limit)
    }

    /// 列出某会话内的消息（时间正序）。
    pub fn list_messages(&self, session_id: &str, limit: u32) -> Result<Vec<MessageRecord>> {
        let db = self.inner.lock_db();
        services::list_messages(&db, session_id, limit)
    }

    /// 删除会话及其全部消息（记忆保留，来源 message_id 置空）。
    pub fn delete_session(&self, id: &str) -> Result<()> {
        let db = self.inner.lock_db();
        services::delete_session(&db, id)
    }

    /// 删除一条记忆。
    pub fn delete_memory(&self, id: &str) -> Result<()> {
        let db = self.inner.lock_db();
        services::delete_memory(&db, id)
    }

    /// 列出全部实体。
    pub fn list_entities(&self) -> Result<Vec<EntityRecord>> {
        let db = self.inner.lock_db();
        services::list_entities(&db)
    }

    /// 列出全部关系。
    pub fn list_relations(&self) -> Result<Vec<RelationRecord>> {
        let db = self.inner.lock_db();
        services::list_relations(&db)
    }

    /// 删除一条关系（记忆手动调整）。
    pub fn delete_relation(&self, id: &str) -> Result<()> {
        let db = self.inner.lock_db();
        services::delete_relation(&db, id)
    }

    /// 删除一个实体。
    pub fn delete_entity(&self, id: &str) -> Result<()> {
        let db = self.inner.lock_db();
        services::delete_entity(&db, id)
    }
}

// ---------- 输入输出（内部 + JSON 载体）类型 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInput {
    pub message: String,
    /// 会话 id。留空时后端自动新建会话并返回新 id。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 显式携带的历史轮次（可选，补充上下文）；留空时后端自动从会话消息构建。
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// 自动构建上下文时取最近的历史消息条数；缺省 6。
    #[serde(default)]
    pub history_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatOutput {
    pub session_id: String,
    pub reply: String,
    /// 本次对话沉淀出的记忆（LLM 判定值得记住时才有）。
    pub memory: Option<MemoryRecord>,
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionOutput {
    pub memory_id: Option<String>,
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

// ---------- UniFFI 导出 ----------

uniffi::setup_scaffolding!();

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
        };
        let output = self.chat_stream(&input, |_| {})?;
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

    #[uniffi::method]
    pub fn list_entities_ffi(&self) -> FfiResult<String> {
        let entities = self.list_entities()?;
        serde_json::to_string(&entities).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn list_relations_ffi(&self) -> FfiResult<String> {
        let relations = self.list_relations()?;
        serde_json::to_string(&relations).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn delete_relation_ffi(&self, id: String) -> FfiResult<()> {
        self.delete_relation(&id).map_err(Into::into)
    }

    #[uniffi::method]
    pub fn delete_entity_ffi(&self, id: String) -> FfiResult<()> {
        self.delete_entity(&id).map_err(Into::into)
    }
}