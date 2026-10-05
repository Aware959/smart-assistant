//! 会话内存运行时：`sessions` / `messages` / `channel_sessions` 的唯一读写方。
//!
//! core 内以具体类型直接接线（会话是应用本体的一份子，不再经过端口抽象）：
//!
//! - **首碰水合 + 进程存活期驻留**：会话首次被触达时把历史消息一次性读进内存，
//!   此后新消息同时写穿 `messages` 表并在内存缓冲累积，生命周期内不再回到 DB
//!   重读历史；
//! - **启动水合映射**：`channel_sessions` 全量读进内存，会话解析 / 重置
//!   都是内存读写 + DB 写穿，反查（channel → session）不再需要 DB join。
//!
//! 共同上界：`Agent::chat_stream` 要求"本条用户消息入库之后再取历史"的时序，
//! 这里 buffer 追加紧随 DB 写穿，天然满足。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::core::intent::{filter, IntentCandidate};
use crate::db::message::Message as DbMessage;

use super::{ConversationSession, MessageTurn, StoredMessage};
use crate::db::{lock_db, Database};
use crate::error::Result;

/// 某通道的入站消息时效分级：决定"这条消息现在该不该回、回得多自然"。
///
/// 根因是长轮询的离线积压——用户发完消息没等到回复就关机/断网，进程重启后
/// 这条消息仍在通道服务端排队。不处理它等于用户凭空失忆（还得再发一次）；
/// 无脑按实时处理又会让 bot 假装刚收到，答非所问。按真实时延分级才像真人。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundFreshness {
    /// 新鲜（≤ 15 分钟）：按实时消息正常回。
    Fresh,
    /// 有延迟（15 分钟 ~ 24 小时）：交给模型自己判断怎么接——
    /// 可以说"刚看到"，也可以顺着话题自然回，不要假装是刚收到的。
    Delayed,
    /// 太久（> 24 小时）：只入库留痕，不触发任何回复。
    TooOld,
}

/// 时效阈值（分钟）：与 [`InboundFreshness::Fresh`] / [`Delayed`] 的分界。
pub const FRESH_WINDOW_MINUTES: i64 = 15;
/// 时效上限（小时）：超过则只入库不回复。
pub const MAX_REPLYABLE_HOURS: i64 = 24;

/// 按消息真实发出时刻与当前时刻的间隔做时效分级。
///
/// 时钟回拨导致的负间隔按 0 处理（视作刚收到）。
pub fn classify_inbound(sent_at: DateTime<Utc>, now: DateTime<Utc>) -> InboundFreshness {
    let age = now.signed_duration_since(sent_at).num_minutes().max(0);
    if age <= FRESH_WINDOW_MINUTES {
        InboundFreshness::Fresh
    } else if age <= MAX_REPLYABLE_HOURS * 60 {
        InboundFreshness::Delayed
    } else {
        InboundFreshness::TooOld
    }
}

type Key = (String, String);

/// 一个已水合、驻留在内存的会话缓冲。
#[derive(Debug, Clone)]
struct HotSession {
    id: String,
    title: String,
    /// 该会话消息全文（与 `messages` 表一致），时间正序。
    messages: Vec<DbMessage>,
}

impl HotSession {
    fn from_db(id: String, title: String, messages: Vec<DbMessage>) -> Self {
        Self {
            id,
            title,
            messages,
        }
    }

    fn append(&mut self, m: DbMessage) {
        self.messages.push(m);
    }
}

#[derive(Default)]
struct Inner {
    /// channel + external → session id（启动即全量水合）。
    channel_map: HashMap<Key, String>,
    /// 已水合的会话缓冲（进程存活期驻留）。
    hot: HashMap<String, HotSession>,
}

/// 会话/消息的内存适配器（由组装层 [`crate::core::app`] 装配，组件直接持引用调用）。
#[derive(Clone)]
pub struct Sessions {
    db: Arc<Mutex<Database>>,
    inner: Arc<Mutex<Inner>>,
}

impl Sessions {
    pub fn new(db: Arc<Mutex<Database>>) -> Result<Self> {
        let channel_map = crate::db::channel::list_mappings(&lock_db(&db))?
            .into_iter()
            .map(|(c, e, sid)| ((c, e), sid))
            .collect();
        Ok(Self {
            db,
            inner: Arc::new(Mutex::new(Inner {
                channel_map,
                hot: HashMap::new(),
            })),
        })
    }

    /// 首碰水合：把 DB 里该会话的历史一次性读进内存。
    fn ensure_hydrated(&self, session_id: &str) -> Result<()> {
        if self.inner.lock().unwrap().hot.contains_key(session_id) {
            return Ok(());
        }
        let (title, msgs) = {
            let db = lock_db(&self.db);
            let title = crate::db::session::get(&db, session_id)?
                .map(|s| s.title)
                .unwrap_or_default();
            let msgs = crate::db::message::list_all(&db, session_id)?;
            (title, msgs)
        };
        let mut inner = self.inner.lock().unwrap();
        if !inner.hot.contains_key(session_id) {
            inner.hot.insert(
                session_id.to_string(),
                HotSession::from_db(session_id.to_string(), title, msgs),
            );
        }
        Ok(())
    }

    /// 已写穿成功的消息回到内存缓冲（会话未水合先水合）。
    fn reflect_message(&self, session_id: &str, m: DbMessage) -> Result<()> {
        self.ensure_hydrated(session_id)?;
        if let Some(hs) = self.inner.lock().unwrap().hot.get_mut(session_id) {
            hs.append(m);
        }
        Ok(())
    }

    /// 反查会话所属的渠道映射（无映射——CLI/HTTP 会话——为 None）。
    pub fn channel_of(&self, session_id: &str) -> Option<(String, String)> {
        let inner = self.inner.lock().unwrap();
        inner
            .channel_map
            .iter()
            .find(|(_, sid)| sid.as_str() == session_id)
            .map(|((c, e), _)| (c.clone(), e.clone()))
    }

    /// 某会话最近一条 user 消息时刻（无则 None）。
    pub fn latest_user_at(&self, session_id: &str) -> Option<DateTime<Utc>> {
        self.ensure_hydrated(session_id).ok()?;
        let inner = self.inner.lock().unwrap();
        inner
            .hot
            .get(session_id)
            .and_then(|hs| hs.messages.iter().rev().find(|m| m.role == "user"))
            .and_then(|m| parse_dt(&m.created_at))
    }

    // ---------- 会话生命周期（原 ConversationStore 端口） ----------

    /// 复用传入的会话 id；不存在或未传则自动新建。
    pub fn get_or_create_session(&self, id: Option<&str>) -> Result<ConversationSession> {
        // 已水合的会话直接走内存，不碰 DB。
        if let Some(id) = id {
            if let Some(hs) = self.inner.lock().unwrap().hot.get(id) {
                return Ok(ConversationSession {
                    id: hs.id.clone(),
                    title: hs.title.clone(),
                });
            }
        }
        let session = match id {
            Some(id) => {
                let db = lock_db(&self.db);
                match crate::db::session::get(&db, id)? {
                    Some(s) => s,
                    None => crate::db::session::create(&db, "")?,
                }
            }
            None => crate::db::session::create(&lock_db(&self.db), "")?,
        };
        self.ensure_hydrated(&session.id)?;
        Ok(ConversationSession {
            id: session.id,
            title: session.title,
        })
    }

    /// 首条消息自动生成会话标题。
    pub fn set_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        crate::db::session::update_title(&lock_db(&self.db), session_id, title)?;
        if let Some(hs) = self.inner.lock().unwrap().hot.get_mut(session_id) {
            hs.title = title.to_string();
        }
        Ok(())
    }

    /// 会话活跃时间戳刷新。
    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        crate::db::session::touch(&lock_db(&self.db), session_id)
    }

    /// 追加一条消息（时间取存储侧"现在"）。
    pub fn add_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<StoredMessage> {
        let m = crate::db::message::create(&lock_db(&self.db), session_id, role, content)?;
        self.reflect_message(session_id, m.clone())?;
        Ok(StoredMessage { id: m.id })
    }

    /// 追加一条消息并指定其真实发出时刻（通道报文时间戳）。
    pub fn add_message_at(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        at: DateTime<Utc>,
    ) -> Result<StoredMessage> {
        let m = crate::db::message::create_at(&lock_db(&self.db), session_id, role, content, at)?;
        self.reflect_message(session_id, m.clone())?;
        Ok(StoredMessage { id: m.id })
    }

    /// 取某个时点之后的全部消息（时间正序）。
    pub fn messages_since(
        &self,
        session_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<MessageTurn>> {
        self.ensure_hydrated(session_id)?;
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .hot
            .get(session_id)
            .map(|hs| {
                hs.messages
                    .iter()
                    .filter(|m| parse_dt(&m.created_at).is_none_or(|t| t >= since))
                    .map(turn_from)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 取最近 N 条消息（时间正序）。
    pub fn recent_messages(&self, session_id: &str, limit: usize) -> Result<Vec<MessageTurn>> {
        self.ensure_hydrated(session_id)?;
        let inner = self.inner.lock().unwrap();
        let out = match inner.hot.get(session_id) {
            Some(_) if limit == 0 => Vec::new(),
            Some(hs) => hs
                .messages
                .iter()
                .rev()
                .take(limit)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(turn_from)
                .collect(),
            None => Vec::new(),
        };
        Ok(out)
    }

    // ---------- 通道映射（原 SessionMap 端口） ----------

    /// 主动开口候选（静默足够久、且距上次 assistant 发言已过间隔门槛）。
    ///
    /// 时间账本直接由 `messages` 派生，不再有独立状态表：候选人从 `channel_sessions`
    /// 取（有会话映射即候选），静默起点取该会话最后一条用户消息的**真实发出时刻**。
    /// 判定规则见 [`crate::core::intent::filter::filter_eligible`]。
    pub fn proactive_candidates(&self, now: DateTime<Utc>) -> Result<Vec<IntentCandidate>> {
        let rows = crate::db::channel::list_proactive_rows(&lock_db(&self.db))?;
        Ok(filter::filter_eligible(rows, now))
    }

    // ---------- 拉取游标（离线积压续传） ----------

    /// 读取某通道上次确认的拉取游标（从未持久化则为 None）。
    pub fn load_cursor(&self, channel: &str) -> Option<String> {
        crate::db::channel::get_cursor(&lock_db(&self.db), channel)
            .ok()
            .flatten()
    }

    /// 持久化某通道的拉取游标（供重启后续传，避免丢弃离线期消息）。
    pub fn save_cursor(&self, channel: &str, cursor: &str) {
        if let Err(e) = crate::db::channel::set_cursor(&lock_db(&self.db), channel, cursor) {
            tracing::warn!(error = %e, channel, "拉取游标持久化失败");
        }
    }

    /// 只入库留痕，不调 LLM、不回复（用于超过时效上限的离线积压消息）。
    ///
    /// 消息仍以**真实时刻**入库，所以下次对方说话时历史时间线是对的。
    pub fn archive_only(
        &self,
        channel: &str,
        external_id: &str,
        text: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let session_id = self.resolve(channel, external_id)?;
        self.add_message_at(&session_id, "user", text, at)?;
        Ok(())
    }

    /// 解析（没有则创建）该通道用户的会话，返回 session id。
    pub fn resolve(&self, channel: &str, external_id: &str) -> Result<String> {
        let key = (channel.to_string(), external_id.to_string());
        if let Some(sid) = self.inner.lock().unwrap().channel_map.get(&key) {
            return Ok(sid.clone());
        }
        let session =
            crate::db::channel::get_or_create_session(&lock_db(&self.db), channel, external_id)?;
        self.ensure_hydrated(&session.id)?;
        let mut inner = self.inner.lock().unwrap();
        inner.channel_map.insert(key, session.id.clone());
        Ok(session.id)
    }

    /// 重置（丢弃旧会话并新建）该通道用户的会话，返回新 session id。
    pub fn reset(&self, channel: &str, external_id: &str) -> Result<String> {
        let key = (channel.to_string(), external_id.to_string());
        let old = self.inner.lock().unwrap().channel_map.get(&key).cloned();
        let session = crate::db::channel::reset_session(&lock_db(&self.db), channel, external_id)?;
        self.ensure_hydrated(&session.id)?;
        let mut inner = self.inner.lock().unwrap();
        inner.channel_map.insert(key, session.id.clone());
        if let Some(old_id) = old {
            if old_id != session.id {
                inner.hot.remove(&old_id);
            }
        }
        Ok(session.id)
    }
}

fn turn_from(m: &DbMessage) -> MessageTurn {
    MessageTurn {
        role: m.role.clone(),
        content: m.content.clone(),
        created_at: m.created_at.clone(),
    }
}

fn parse_dt(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn db() -> Arc<Mutex<Database>> {
        Arc::new(Mutex::new(Database::in_memory().unwrap()))
    }

    #[test]
    fn add_and_recent_messages_single_source() {
        let sessions = Sessions::new(db()).unwrap();
        let s = sessions.get_or_create_session(None).unwrap();
        sessions.add_message(&s.id, "user", "早").unwrap();
        sessions.add_message(&s.id, "assistant", "好").unwrap();

        let recent = sessions.recent_messages(&s.id, 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].role, "user");
        assert_eq!(recent[1].role, "assistant");

        // 写穿验证：DB 里只有这两个会话消息。
        let db = lock_db(&sessions.db);
        let all = crate::db::message::list_all(&db, &s.id).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn hydrate_loads_history_from_db() {
        let db = db();
        let sessions = Sessions::new(db.clone()).unwrap();
        let s = sessions.get_or_create_session(None).unwrap();
        sessions.add_message(&s.id, "user", "第一条").unwrap();
        sessions.add_message(&s.id, "assistant", "第二条").unwrap();

        // 新进程视角：新建适配器（同一 DB）→ 首碰水合读全历史。
        let fresh = Sessions::new(db).unwrap();
        let recent = fresh.recent_messages(&s.id, 100).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].content, "第一条");

        // 水合后可被 get_or_create 直接命中，浏览器进程存活期内不再读 DB。
        let got = fresh.get_or_create_session(Some(&s.id)).unwrap();
        assert_eq!(got.id, s.id);
    }

    #[test]
    fn session_map_resolve_sets_channel_and_latest_time() {
        let sessions = Sessions::new(db()).unwrap();
        let sid = sessions.resolve("telegram", "100").unwrap();
        sessions
            .add_message_at(
                &sid,
                "user",
                "你好",
                chrono::DateTime::parse_from_rfc3339("2026-09-14T03:47:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .unwrap();

        assert_eq!(
            sessions.channel_of(&sid),
            Some(("telegram".into(), "100".into()))
        );
        assert!(sessions.latest_user_at(&sid).is_some());
    }

    #[test]
    fn freshness_buckets_split_by_real_age() {
        let now = Utc::now();
        // 刚发出 → 正常回
        assert_eq!(classify_inbound(now, now), InboundFreshness::Fresh);
        assert_eq!(
            classify_inbound(now - Duration::minutes(FRESH_WINDOW_MINUTES), now),
            InboundFreshness::Fresh
        );
        // 15 分钟 ~ 24 小时 → 交给模型判断怎么接
        assert_eq!(
            classify_inbound(now - Duration::minutes(FRESH_WINDOW_MINUTES + 1), now),
            InboundFreshness::Delayed
        );
        assert_eq!(
            classify_inbound(now - Duration::hours(MAX_REPLYABLE_HOURS), now),
            InboundFreshness::Delayed
        );
        // 超过 24 小时 → 只入库不回复
        assert_eq!(
            classify_inbound(now - Duration::hours(MAX_REPLYABLE_HOURS + 1), now),
            InboundFreshness::TooOld
        );
        // 时钟回拨：负间隔视作刚收到，不该误判成 TooOld
        assert_eq!(
            classify_inbound(now + Duration::hours(3), now),
            InboundFreshness::Fresh
        );
    }

    /// 离线积压的过期消息只入库，不建 AI 回复、不产生 assistant 消息，
    /// 但仍以真实时刻落库——下次对方说话时历史时间线是对的。
    #[test]
    fn archive_only_stores_with_real_time_and_no_reply() {
        let sessions = Sessions::new(db()).unwrap();
        let sent_at = Utc::now() - Duration::hours(30);
        sessions
            .archive_only("telegram", "100", "三天前那条", sent_at)
            .unwrap();

        let sid = sessions.resolve("telegram", "100").unwrap();
        let recent = sessions.recent_messages(&sid, 10).unwrap();
        assert_eq!(recent.len(), 1, "只应留下用户消息本身");
        assert_eq!(recent[0].role, "user");
        assert_eq!(recent[0].content, "三天前那条");
        assert_eq!(
            sessions.latest_user_at(&sid),
            Some(sent_at),
            "应以真实发出时刻入库，而非入库时刻"
        );
    }

    /// 游标持久化：重启后的新实例能续上上次位置（离线消息不丢的关键）。
    #[test]
    fn cursor_persists_across_instances() {
        let db = db();
        let first = Sessions::new(db.clone()).unwrap();
        assert!(first.load_cursor("telegram").is_none(), "首次运行无游标");
        first.save_cursor("telegram", "4821");
        first.save_cursor("ilink", "buf-abc");

        // 新进程视角：同一 DB 重新装配 → 游标可见。
        let restarted = Sessions::new(db).unwrap();
        assert_eq!(restarted.load_cursor("telegram").as_deref(), Some("4821"));
        assert_eq!(restarted.load_cursor("ilink").as_deref(), Some("buf-abc"));
        // 通道间互不干扰。
        assert!(restarted.load_cursor("other").is_none());
    }

    #[test]
    fn reset_switches_session_and_drops_old_buffer() {
        let sessions = Sessions::new(db()).unwrap();
        let old = sessions.resolve("telegram", "100").unwrap();
        sessions.add_message(&old, "user", "旧上下文").unwrap();

        let new_id = sessions.reset("telegram", "100").unwrap();
        assert_ne!(new_id, old);

        // 新会话重启上下文：旧消息不再可见。
        assert!(sessions.recent_messages(&new_id, 10).unwrap().is_empty());
        assert!(sessions.latest_user_at(&new_id).is_none());
    }
}
