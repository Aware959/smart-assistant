//! AI 自我属性适配器：读取 `self_profile` KV 表的人设，渲染为提示词区块。
//!
//! 与 [`crate::core::memory`] 的"用户记忆"相对：这里是**关于 AI 自己的**稳定属性
//! （性格/人格/三观/爱好…）。量少、准静态：整表只有 20 行上下，却每轮对话 / 每次
//! 主动决策都要读一次并重新渲染，是纯开销。与回忆注入路径互不干扰。
//!
//! 读路径：`block()` 命中 TTL 缓存时直接返回上次渲染结果，不再碰 DB；未命中或已过期
//! 才持锁读完整张小表 → 渲染成「性格：…；人格：…」纯文本 → 立即释放锁，
//! **绝不跨 LLM 调用持锁**（与历史/召回同一约束）。
//! 写路径 v1 无接线（人工改库）；aiworld 重设计后 AI 可经
//! [`crate::db::self_profile`] 的 `set` / `delete` 自我演化。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::db::Database;

/// 渲染结果的复用时长。
///
/// 人设是准静态的（20 行，一个月未必改一次），而 `block()` 每轮对话 + 每次主动决策
/// 各调一次。给一个短 TTL：连续对话期间基本全命中，同时把「绕过代码直接改库」
/// 造成的不一致限制在 TTL 之内——`set` / `delete` 目前无业务接线，属性靠人工维护，
/// 没有可靠的写入时刻可以挂钩，所以失效走时间而不是版本号。
const CACHE_TTL: Duration = Duration::from_secs(10);

/// 上次渲染的结果与其时刻。
struct Cached {
    at: Instant,
    text: String,
}

/// 自我属性适配器：DB 句柄的持有者 + 渲染结果缓存。
pub struct Profile {
    db: Arc<Mutex<Database>>,
    ttl: Duration,
    cache: Mutex<Option<Cached>>,
}

impl Profile {
    pub(crate) fn new(db: Arc<Mutex<Database>>) -> Self {
        Self::with_ttl(db, CACHE_TTL)
    }

    /// 指定复用时长。测试用它把过期窗口调到 0 / 很长，从而确定性地验证命中与失效。
    fn with_ttl(db: Arc<Mutex<Database>>, ttl: Duration) -> Self {
        Self {
            db,
            ttl,
            cache: Mutex::new(None),
        }
    }

    /// 渲染全部属性为「key：value」按插入序以「；」连接的文本。
    ///
    /// TTL 内重复调用直接返回缓存（不持 DB 锁）；表为空或读取失败时返回空串
    /// （调用方整体省略人设区块，回落无角色基线）。
    pub(crate) fn block(&self) -> String {
        if let Some(text) = self.cached() {
            return text;
        }
        let text = self.render();
        self.store(text.clone());
        text
    }

    /// 读缓存：命中且未过期则返回 Some。
    fn cached(&self) -> Option<String> {
        let ttl = self.ttl;
        let guard = self.cache.lock().ok()?;
        let cached = guard.as_ref()?;
        (cached.at.elapsed() < ttl).then(|| cached.text.clone())
    }

    fn store(&self, text: String) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(Cached {
                at: Instant::now(),
                text,
            });
        }
    }

    /// 真读一次 DB 并渲染。持锁期间只做「读小表 + 拼字符串」，不做任何 IO 等待。
    fn render(&self) -> String {
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

#[cfg(test)]
#[path = "self_profile_tests.rs"]
mod self_profile_tests;
