//! AI 自我属性适配器：读取当前身份快照，渲染为提示词区块。
//!
//! 与 [`crate::core::memory`] 的"用户记忆"相对：这里是**关于 AI 自己的**稳定属性
//! （性格/人格/三观/爱好…）。量少、准静态：整表只有 20 行上下，却每轮对话 / 每次
//! 主动决策都要读一次并重新渲染，是纯开销。与回忆注入路径互不干扰。
//!
//! 读路径：优先读取最新 `self_profile_versions` 快照并按 canonical 字段顺序渲染；
//! 尚无版本记录（或快照损坏）时回退到旧 `self_profile` KV 的插入序渲染，保证升级兼容。
//! `snapshot()` 命中 TTL 缓存时直接返回上次渲染结果与身份版本，不再碰 DB；未命中或
//! 已过期才持锁读取 → 渲染 → 立即释放锁，**绝不跨 LLM 调用持锁**。
//! 写路径 v1 无接线（人工改库）；身份演化生效后由调用方显式 [`Profile::invalidate`]，
//! 不等 TTL 过期，保证下一轮提示词立即使用新快照。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::core::identity::AgentIdentity;
use crate::db::Database;

/// 渲染结果的复用时长。
///
/// 人设是准静态的（20 行，一个月未必改一次），而 `snapshot()` 每轮对话 + 每次主动决策
/// 各调一次。给一个短 TTL：连续对话期间基本全命中，同时把「绕过代码直接改库」
/// 造成的不一致限制在 TTL 之内——`set` / `delete` 目前无业务接线，属性靠人工维护，
/// 没有可靠的写入时刻可以挂钩，所以失效走时间而不是版本号。
const CACHE_TTL: Duration = Duration::from_secs(10);

/// 上次渲染的结果与其时刻。
struct Cached {
    at: Instant,
    version: Option<u64>,
    text: String,
}

/// 一次提示词渲染使用的身份快照。
///
/// `version = None` 表示当前仍是尚未写入审计表的旧 KV 数据，或版本快照无法解析后
/// 回退到了旧数据。`text` 为空时调用方整体省略人设区块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileSnapshot {
    pub version: Option<u64>,
    pub text: String,
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

    /// 读取当前身份快照与版本。
    ///
    /// TTL 内重复调用直接返回缓存（不持 DB 锁）；表为空或读取失败时返回空串
    /// （调用方整体省略人设区块，回落无角色基线）。
    pub(crate) fn snapshot(&self) -> ProfileSnapshot {
        if let Some(snapshot) = self.cached() {
            return snapshot;
        }
        let snapshot = self.render();
        self.store(snapshot.clone());
        snapshot
    }

    /// 丢弃当前渲染缓存。
    ///
    /// 身份变更在事务提交后调用；下一轮 `snapshot()` 会立即重读最新身份版本，而不是
    /// 等待 TTL。人工改库仍由 TTL 兜底。
    pub(crate) fn invalidate(&self) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = None;
        }
    }

    /// 读缓存：命中且未过期则返回 Some。
    fn cached(&self) -> Option<ProfileSnapshot> {
        let ttl = self.ttl;
        let guard = self.cache.lock().ok()?;
        let cached = guard.as_ref()?;
        (cached.at.elapsed() < ttl).then(|| ProfileSnapshot {
            version: cached.version,
            text: cached.text.clone(),
        })
    }

    fn store(&self, snapshot: ProfileSnapshot) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(Cached {
                at: Instant::now(),
                version: snapshot.version,
                text: snapshot.text,
            });
        }
    }

    /// 真读一次 DB 并渲染。持锁期间只做「读小表 + 拼字符串」，不做任何 IO 等待。
    fn render(&self) -> ProfileSnapshot {
        let guard = crate::db::lock_db(&self.db);
        match crate::db::identity::latest_version_on(guard.conn()) {
            Ok(Some(version)) => match version_number(version.version) {
                Some(number) => match AgentIdentity::from_snapshot_json(
                    number,
                    version.created_at,
                    &version.snapshot,
                ) {
                    Ok(identity) => ProfileSnapshot {
                        version: Some(number),
                        text: identity.render_block(),
                    },
                    Err(e) => {
                        tracing::warn!(
                            identity_version = number,
                            error = %e,
                            "身份快照无效，回退到旧 self_profile 渲染"
                        );
                        self.render_legacy(guard.conn(), None)
                    }
                },
                None => {
                    tracing::warn!(
                        identity_version = version.version,
                        "身份版本号无效，回退到旧 self_profile 渲染"
                    );
                    self.render_legacy(guard.conn(), None)
                }
            },
            Ok(None) => self.render_legacy(guard.conn(), Some(AgentIdentity::UNVERSIONED)),
            Err(e) => {
                tracing::warn!(error = %e, "身份版本读取失败，本轮按无人设继续");
                ProfileSnapshot {
                    version: None,
                    text: String::new(),
                }
            }
        }
    }

    /// 旧 KV 兼容渲染：保持插入序和原 key，避免升级时改变现有提示词。
    fn render_legacy(&self, conn: &rusqlite::Connection, version: Option<u64>) -> ProfileSnapshot {
        match crate::db::self_profile::all(conn) {
            Ok(rows) => ProfileSnapshot {
                version,
                text: rows
                    .iter()
                    .map(|(key, value)| format!("{key}：{value}"))
                    .collect::<Vec<_>>()
                    .join("；"),
            },
            Err(e) => {
                tracing::warn!(error = %e, "self_profile 读取失败，本轮按无人设继续");
                ProfileSnapshot {
                    version: None,
                    text: String::new(),
                }
            }
        }
    }
}

/// 把审计表里的非负版本号转成领域模型使用的 `u64`。
fn version_number(version: i64) -> Option<u64> {
    u64::try_from(version)
        .ok()
        .filter(|v| *v >= AgentIdentity::INITIAL_VERSION)
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
