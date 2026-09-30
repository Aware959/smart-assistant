//! AI 自我属性适配器：读取 `self_profile` KV 表的人设，渲染为提示词区块。
//!
//! 与 [`crate::core::memory`] 的"用户记忆"相对：这里是**关于 AI 自己的**稳定属性
//! （性格/人格/三观/爱好…）。量少、全量常驻直读（每轮对话/决策前读一次），
//! 与回忆注入路径互不干扰。
//!
//! 读路径：`block()` 持 DB 锁读完整张小表 → 渲染成「性格：…；人格：…」纯文本 →
//! 立即释放锁，**绝不跨 LLM 调用持锁**（与历史/召回同一约束）。
//! 写路径 v1 无接线（人工改库）；aiworld 重设计后 AI 可经
//! [`crate::db::self_profile`] 的 `set` / `delete` 自我演化。

use std::sync::{Arc, Mutex};

use crate::db::Database;

/// 自我属性适配器：只是 DB 句柄的持有者，读取即渲染。
pub struct Profile {
    db: Arc<Mutex<Database>>,
}

impl Profile {
    pub(crate) fn new(db: Arc<Mutex<Database>>) -> Self {
        Self { db }
    }

    /// 渲染全部属性为「key：value」按插入序以「；」连接的文本。
    ///
    /// 表为空或读取失败时返回空串（调用方整体省略人设区块，回落无角色基线）。
    pub(crate) fn block(&self) -> String {
        let guard = crate::db::lock_db(&self.db);
        match crate::db::self_profile::all(guard.conn()) {
            Ok(rows) => rows
                .iter()
                .map(|(key, value)| format!("{key}：{value}"))
                .collect::<Vec<_>>()
                .join("；"),
            Err(e) => {
                tracing::warn!(error = %e, "self_profile 读取失败，本轮按无人设继续");
                String::new()
            }
        }
    }
}
