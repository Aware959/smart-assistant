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

use super::{ConversationSession, MessageTurn, StoredMessage};
use crate::db::{lock_db, Database};
use crate::db::message::Message as DbMessage;
use crate::error::Result;

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
    pub fn add_message(&self, session_id: &str, role: &str, content: &str) -> Result<StoredMessage> {
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
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        sessions.add_message_at(
            &sid,
            "user",
            "你好",
            chrono::DateTime::parse_from_rfc3339("2026-09-14T03:47:00+00:00")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap();

        assert_eq!(sessions.channel_of(&sid), Some(("telegram".into(), "100".into())));
        assert!(sessions.latest_user_at(&sid).is_some());
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