//! 应用组装层（composition root）：把依赖装配起来，向外部交出自带能力的服务。
//!
//! 它不做任何业务转发——装配 db → 适配器 → 注入主线，然后把装配结果以 pub 字段
//! 交出去，组件（api / channels / intent）直接持服务引用调各服务自己的方法：
//!
//! - [`crate::core::agent::Agent`]（`chat`）：**主线聊天编排服务**，外部唯一认可的
//!   聊天接口（对话 / 记忆写入 / 纯 LLM 决策 / 上下文取数）；
//! - [`crate::core::session::Sessions`]（`session`）：会话服务（入站解析 / 重置 / 消息落库 /
//!   主动候选派生）。
//!
//! db 不走壳：各服务经装配对象拿到句柄，能力方法全部收敛在服务上，调用方拿不到
//! 裸数据连接。本层不提供管理门面——对外能力只剩主线聊天（`chat`）。

use std::sync::{Arc, Mutex};

use crate::error::Result;

/// 应用组装层。只装配，不转发。
pub struct App {
    /// 主线聊天编排服务（对话 / 记忆写入 / 纯 LLM 决策 / 上下文取数）。
    pub chat: Arc<crate::core::agent::Agent>,
    /// 会话服务（入站解析 / 重置 / 消息落库）。
    pub session: Arc<crate::core::session::Sessions>,
}

impl App {
    /// 打开或创建数据库、初始化 schema 后装配。
    pub fn new(db_path: &str) -> Result<Self> {
        Self::assemble(Arc::new(Mutex::new(crate::db::Database::open(db_path)?)))
    }

    pub fn new_in_memory() -> Result<Self> {
        Self::assemble(Arc::new(Mutex::new(crate::db::Database::in_memory()?)))
    }

    /// 装配主线路：会话 / 能力（Llm、记忆 Store、自我属性 Profile）→ 注入主线 Agent。
    fn assemble(db: Arc<Mutex<crate::db::Database>>) -> Result<Self> {
        let session = Arc::new(crate::core::session::Sessions::new(db.clone())?);
        let llm = Arc::new(crate::llm::chat::Llm);
        let memory = Arc::new(crate::core::memory::Store::new(db.clone()));
        let profile = Arc::new(crate::core::self_profile::Profile::new(db));
        // 贴纸目录是文件不是状态：装配时读一次，之后 `sa-sticker fetch` 拉的新目录
        // 要重启进程才生效。文件不存在 = 没拉过 = 贴纸能力关闭，不是错误。
        let stickers = crate::core::sticker::StickerCatalog::load_default();
        if !stickers.is_empty() {
            tracing::info!(
                set = %stickers.set,
                n = stickers.stickers.len(),
                "贴纸目录已加载"
            );
        }
        let chat = Arc::new(crate::core::agent::Agent::new(
            session.clone(),
            llm,
            memory,
            profile,
            stickers,
        ));
        Ok(Self { chat, session })
    }
}
