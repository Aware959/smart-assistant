//! 应用外壳：持有编排内核 [`crate::core::agent::Agent`] 与数据库句柄。
//!
//! 数据库由宿主层（本层）持有，经 `Arc<Mutex<Database>>` 注入各能力适配器；
//! 内核本身不感知存储。本层对外提供能力门面，仅做薄委托与类型映射，
//! FFI 导出见 [`crate::ffi`]。
//!
//! 组件层（`channels` / `proactive` / `world`）不再 import 本层的 [`Assistant`]：
//! 它们只依赖 [`crate::host::Host`] 窄端口（依赖倒置的锚点），由本层实现并注入。

use std::sync::{Arc, Mutex, MutexGuard};

use crate::core::ports::ChatLlm;
use crate::core::types::{ChatInput, MemoryHit, MemoryRecord};
use crate::error::Result;
use crate::host::Host;
use crate::services;
use crate::services::{MessageRecord, SessionRecord};

#[derive(uniffi::Object)]
pub struct Assistant {
    db: Arc<Mutex<crate::db::Database>>,
    inner: crate::core::agent::Agent,
}

impl Assistant {
    /// 打开或创建数据库，并初始化 schema。
    pub fn new(db_path: &str) -> Result<Self> {
        Self::with_db_arc(Arc::new(Mutex::new(crate::db::Database::open(db_path)?)))
    }

    pub fn new_in_memory() -> Result<Self> {
        Self::with_db_arc(Arc::new(Mutex::new(crate::db::Database::in_memory()?)))
    }

    /// 由宿主持有的数据库句柄装配默认适配器，注入内核。
    fn with_db_arc(db: Arc<Mutex<crate::db::Database>>) -> Result<Self> {
        let inner = crate::core::agent::Agent::with_ports(
            Arc::new(crate::services::Conversation::new(db.clone())),
            Arc::new(crate::llm::chat::Llm),
            Arc::new(crate::memory::store::Store::new(db.clone())),
            Arc::new(crate::world::World::new(db.clone())),
        );
        Ok(Self { db, inner })
    }

    /// 数据库锁守卫。仅限同层服务门面 / Host 实现内部使用，勿跨 `.await` 持锁。
    /// 带等待耗时诊断与毒锁自愈（见 [`crate::db::lock_db`]）。
    fn lock_db(&self) -> MutexGuard<'_, crate::db::Database> {
        crate::db::lock_db(&self.db)
    }

    /// 兼容旧调用点的(crate 内)数据库访问入口，供 channels / proactive / world
    /// 在完全迁移到 `Host` 窄端口前复用内部连接。与 [`Host::db`] 同一能力。
    pub(crate) fn inner_db(&self) -> MutexGuard<'_, crate::db::Database> {
        self.lock_db()
    }

    /// 对话主入口（流式）：完整流水线见 [`crate::core::agent::Agent::chat_stream`]。
    pub fn chat_stream<F>(&self, input: &ChatInput, on_delta: F) -> Result<crate::core::types::ChatOutput>
    where
        F: FnMut(&str) + Send,
    {
        self.inner.chat_stream(input, on_delta)
    }

    /// 回复交付后的记忆沉淀（判定 → 关系演化 → 落库），阻塞调用。
    ///
    /// 记忆沉淀刻意不挂在 [`Self::chat_stream`] 上：判定要调 LLM、沉淀要调 embedding，
    /// 会让首字延迟变长，失败还会连累回复。调用方应在回复送达之后调用本方法
    /// （宜放 `spawn_blocking`），并自行决定是否用返回的记忆做展示。
    pub fn settle_memory(
        &self,
        session_id: &str,
        user_message_id: &str,
        text: &str,
    ) -> Option<MemoryRecord> {
        self.inner.settle_memory(session_id, user_message_id, text)
    }

    // ---------- 服务门面（委托 services 层） ----------

    /// 语义检索记忆。
    pub fn search_memory(&self, query: &str, limit: u32) -> Result<Vec<MemoryHit>> {
        let db = self.lock_db();
        services::search_memory(&db, query, limit)
    }

    /// 手动添加一条记忆，返回记忆 id。
    pub fn add_memory(
        &self,
        content: &str,
        memory_type: &str,
        message_id: Option<&str>,
    ) -> Result<String> {
        let db = self.lock_db();
        services::add_memory(&db, content, memory_type, message_id)
    }

    /// 列出全部记忆。
    pub fn list_memories(&self, limit: u32) -> Result<Vec<MemoryRecord>> {
        let db = self.lock_db();
        services::list_memories(&db, limit)
    }

    /// 新建会话，返回会话 id。
    pub fn create_session(&self, title: &str) -> Result<String> {
        let db = self.lock_db();
        services::create_session(&db, title)
    }

    /// 列出会话（按最近更新倒序）。
    pub fn list_sessions(&self, limit: u32) -> Result<Vec<SessionRecord>> {
        let db = self.lock_db();
        services::list_sessions(&db, limit)
    }

    /// 列出某会话内的消息（时间正序）。
    pub fn list_messages(&self, session_id: &str, limit: u32) -> Result<Vec<MessageRecord>> {
        let db = self.lock_db();
        services::list_messages(&db, session_id, limit)
    }

    /// 删除会话及其全部消息（记忆保留，来源 message_id 置空）。
    pub fn delete_session(&self, id: &str) -> Result<()> {
        let db = self.lock_db();
        services::delete_session(&db, id)
    }

    /// 删除一条记忆。
    pub fn delete_memory(&self, id: &str) -> Result<()> {
        let db = self.lock_db();
        services::delete_memory(&db, id)
    }
}

/// 宿主窄端口实现：把组件需要的四张能力委托给内核与数据库。
///
/// 组件层统一依赖 [`crate::host::Host`]，由本层（组合根）装配并注入。
impl Host for Assistant {
    fn db(&self) -> MutexGuard<'_, crate::db::Database> {
        self.lock_db()
    }

    fn chat_stream(
        &self,
        input: &ChatInput,
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<crate::core::types::ChatOutput> {
        self.inner.chat_stream(input, |delta| on_delta(delta))
    }

    fn settle_memory(
        &self,
        session_id: &str,
        user_message_id: &str,
        text: &str,
    ) -> Option<MemoryRecord> {
        self.inner.settle_memory(session_id, user_message_id, text)
    }

    fn chat_llm(&self) -> Arc<dyn ChatLlm> {
        self.inner.chat_llm()
    }

    fn world_snapshot_text(&self, session_id: &str) -> String {
        self.inner.world_snapshot_text(session_id)
    }
}
