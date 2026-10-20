//! 会话内存运行时：`sessions` / `messages` / `channel_sessions` 的唯一读写方。
//!
//! core 内以具体类型直接接线（会话是应用本体的一份子，不再经过端口抽象）：
//!
//! - **首碰水合 + 进程存活期驻留**：会话首次被触达时把历史消息一次性读进内存，
//!   此后新消息同时写穿 `messages` 表并在内存缓冲累积，生命周期内不再回到 DB
//!   重读历史；
//! - **启动水合映射**：`channel_sessions` 全量读进内存，会话解析 / 重置
//!   都是内存读写 + DB 写穿，反查（channel → session）不再需要 DB join；
//! - **运行中轮次登记**：同一会话同时只跑一轮对话（[`Sessions::begin_turn`]）。
//!   一轮在跑时新到的用户消息**照常落库**，由那一轮自己在迭代边界取用
//!   （[`Sessions::user_messages_after`]）——这就是"agent 跑着也能插话"的全部机制：
//!   通道只负责把消息写进时间线，模型下一轮生成前把它读进 live 上下文。
//!
//! 共同上界：**读历史一律读内存缓冲**（`messages_since` / `recent_messages` /
//! `user_messages_after` 都是），所以消息一落 `messages` 表就必须同时进缓冲——
//! 写穿与缓冲追加在本模块内成对出现，凡写消息的入口都走这里，天然满足。

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
    /// 正在跑的对话轮次：session id → 令牌号（见 [`Sessions::begin_turn`]）。
    turns: HashMap<String, u64>,
    /// 轮次令牌号发号器（单调递增，用来区分"上一轮"和"下一轮"）。
    next_turn_token: u64,
}

/// 会话/消息的内存适配器（由组装层 [`crate::core::app`] 装配，组件直接持引用调用）。
#[derive(Clone)]
pub struct Sessions {
    db: Arc<Mutex<Database>>,
    inner: Arc<Mutex<Inner>>,
}

/// 一轮对话的占用凭证：持有期间该会话被视为"有轮次在跑"，丢弃时销号。
///
/// 轮次结束的每条路径（正常收尾、报错、超时）都会丢弃它，所以会话不会被永久占住。
/// 令牌号用于销号时认人：超时轮次的凭证可能与随后新起的轮次同时在场（超时只取消异步
/// 侧的等待，阻塞线程里那一轮还会跑一会儿），销号必须只销自己那一号。
pub struct TurnTicket {
    owner: Sessions,
    session_id: String,
    token: u64,
}

impl Drop for TurnTicket {
    fn drop(&mut self) {
        // 锁中毒说明进程已处于异常态，这里不该再 panic（Drop 里 panic 会二次中断）。
        if let Ok(mut inner) = self.owner.inner.lock() {
            if inner.turns.get(&self.session_id) == Some(&self.token) {
                inner.turns.remove(&self.session_id);
            }
        }
    }
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
                turns: HashMap::new(),
                next_turn_token: 0,
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
    ///
    /// **写库之前必须先水合**（见 [`Self::add_message`] 系列），否则水合快照会把刚插入
    /// 的这行一并读进来，这里再 `append` 一次，同一 id 就在缓冲里存了两份。后果不是多占
    /// 点内存：插话游标 [`Self::user_messages_after`] 用 `position()` 找**第一份**，
    /// 第二份会在每一轮迭代里被当成"新插话"重复并入上下文，直到撞上工具循环的迭代上限
    /// （表现为模型以为用户把同一句话发了好几遍）。这里再按 id 兜底去重。
    fn reflect_message(&self, session_id: &str, m: DbMessage) -> Result<()> {
        self.ensure_hydrated(session_id)?;
        if let Some(hs) = self.inner.lock().unwrap().hot.get_mut(session_id) {
            if hs.messages.iter().all(|x| x.id != m.id) {
                hs.append(m);
            }
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
        // 水合必须排在写库之前：反过来时 `reflect_message` 里的水合快照会带上刚插入的
        // 这行，再 append 一次即重复（详见 [`Self::reflect_message`]）。
        self.ensure_hydrated(session_id)?;
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
        self.ensure_hydrated(session_id)?;
        let m = crate::db::message::create_at(&lock_db(&self.db), session_id, role, content, at)?;
        self.reflect_message(session_id, m.clone())?;
        Ok(StoredMessage { id: m.id })
    }

    /// 追加一条带工具字段的消息（工具往返：assistant 轮的工具调用列表、或 tool 结果
    /// 的调用 id 与工具名）。工具结果以 `role='tool'` + `tool_call_id` 落库。
    pub fn add_message_full(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        meta: &crate::db::message::ToolMeta,
    ) -> Result<StoredMessage> {
        // 同 `add_message`：先水合，避免水合快照把刚插入的这行再算一遍。
        self.ensure_hydrated(session_id)?;
        let m = crate::db::message::create_full(
            &lock_db(&self.db),
            session_id,
            role,
            content,
            chrono::Utc::now(),
            meta,
        )?;
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

    // ---------- 运行中轮次（插话） ----------

    /// 登记"本会话的一轮对话开始了"，返回该轮的占用凭证；已有轮次在跑则返回 `None`。
    ///
    /// 调用方据此决定：拿到凭证就起一轮；拿不到就**只把消息落库**——运行中的那一轮会在
    /// 下一个迭代边界自己把它读进上下文（见 [`Self::user_messages_after`]）。这样同一会话
    /// 永远是"一个 agent 循环 + 期间不断并入的新消息"，而不是几轮并发各答各的。
    pub fn begin_turn(&self, session_id: &str) -> Option<TurnTicket> {
        let mut inner = self.inner.lock().unwrap();
        if inner.turns.contains_key(session_id) {
            return None;
        }
        let token = inner.next_turn_token;
        inner.next_turn_token = token.wrapping_add(1);
        inner.turns.insert(session_id.to_string(), token);
        drop(inner);
        Some(TurnTicket {
            owner: self.clone(),
            session_id: session_id.to_string(),
            token,
        })
    }

    /// 取某个会话里排在 `after_id` **之后**的全部用户消息（时间正序）。
    ///
    /// 运行中的轮次用它拿运行期间新到的插话：插话一到通道就落库了，所以这里只读不消费，
    /// 轮次中途失败也不会把插话弄丢——下一次对话（或重试）照样从时间线里读得到。
    ///
    /// `after_id` 不在缓冲里时返回空：水位都认不准，宁可这一轮不多并，也不把整段历史
    /// 重灌进上下文。
    pub fn user_messages_after(
        &self,
        session_id: &str,
        after_id: &str,
    ) -> Result<Vec<MessageTurn>> {
        self.ensure_hydrated(session_id)?;
        let inner = self.inner.lock().unwrap();
        let Some(hs) = inner.hot.get(session_id) else {
            return Ok(Vec::new());
        };
        let Some(pos) = hs.messages.iter().position(|m| m.id == after_id) else {
            return Ok(Vec::new());
        };
        Ok(hs.messages[pos + 1..]
            .iter()
            .filter(|m| m.role == "user")
            .map(turn_from)
            .collect())
    }

    /// 会话里比 `trigger_id` 更新的最后一条用户消息（就是它自己、或没有用户消息则 `None`）。
    ///
    /// 失败轮次要重跑的目标由它决定：重试按触发消息截断历史（见
    /// [`crate::core::context::fetch_history`] 的排除语义），拿旧触发消息重跑会把运行期间
    /// 收到的插话整段排除在上下文之外——而那恰恰是最该被回答的那条。
    pub fn newer_user_message(
        &self,
        session_id: &str,
        trigger_id: &str,
    ) -> Result<Option<MessageTurn>> {
        self.ensure_hydrated(session_id)?;
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .hot
            .get(session_id)
            .and_then(|hs| hs.messages.iter().rev().find(|m| m.role == "user"))
            .filter(|m| m.id != trigger_id)
            .map(turn_from))
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

    // ---------- 失败轮次重试队列（pending_retry） ----------

    /// 入队一条待重试记录（对话轮次因内部错误失败后调用，不再向用户发声）。
    ///
    /// 记录 id / 初始尝试次数 / 创建时刻在这里统一生成，通道只给"重试哪个会话的哪条消息、
    /// 隔多久再来"。`delay_secs` 是首轮重试的退避秒数，退避策略本身归通道定——通道层不许
    /// 碰 `db`（见 `tests/layer_guard.rs`），所以只传时长不传记录。
    pub fn enqueue_retry_for(
        &self,
        session_id: &str,
        chat_id: &str,
        user_message_id: &str,
        content: &str,
        user_time: Option<String>,
        delay_secs: i64,
    ) -> Result<()> {
        let now = Utc::now();
        let row = crate::db::pending_retry::PendingRetry {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            chat_id: chat_id.to_string(),
            user_message_id: user_message_id.to_string(),
            content: content.to_string(),
            user_time,
            attempts: 0,
            next_attempt_at: now + chrono::Duration::seconds(delay_secs),
            created_at: now,
        };
        crate::db::pending_retry::enqueue(&lock_db(&self.db), &row)
    }

    /// 取所有已到重试时刻的待重试记录（按时间正序）。
    pub fn due_retries(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<crate::db::pending_retry::PendingRetry>> {
        crate::db::pending_retry::due(&lock_db(&self.db), &now)
    }

    /// 推进重试计数并设定下次重试时刻（本次重试仍失败、但还没到上限时）。
    pub fn mark_retry_attempted(&self, id: &str, next_attempt_at: DateTime<Utc>) -> Result<()> {
        crate::db::pending_retry::mark_attempted(&lock_db(&self.db), id, &next_attempt_at)
    }

    /// 删除一条待重试记录（重试成功、或放弃重试时）。
    pub fn remove_retry(&self, id: &str) -> Result<()> {
        crate::db::pending_retry::remove(&lock_db(&self.db), id)
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
        id: m.id.clone(),
        role: m.role.clone(),
        content: m.content.clone(),
        created_at: m.created_at.clone(),
        tool_calls: m.tool_calls.clone(),
        tool_call_id: m.tool_call_id.clone(),
        tool_name: m.tool_name.clone(),
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
    fn fetch_excludes_retried_user_message_and_its_partial_turns() {
        let sessions = Sessions::new(db()).unwrap();
        let s = sessions.get_or_create_session(None).unwrap();
        // 早前的一轮正常对话。
        sessions.add_message(&s.id, "user", "前面的话").unwrap();
        sessions
            .add_message(&s.id, "assistant", "前面的回复")
            .unwrap();
        // 失败轮次：用户消息已落库，且该轮其后半截又产生了 assistant/tool 往返。
        // 往返按协议形态落库（assistant 带调用声明、tool 带 call_id）：读历史时会按 call_id
        // 校验配对并净化残缺结构，所以这里造的是"失败轮次里已经发出去的完整往返"。
        let failed = sessions
            .add_message(&s.id, "user", "这条要不要重试")
            .unwrap();
        let calls = serde_json::to_string(&vec![crate::core::schema::message::ChatCall::new(
            "c1",
            "send_message",
            serde_json::json!({}),
        )])
        .unwrap();
        sessions
            .add_message_full(
                &s.id,
                "assistant",
                "半截",
                &crate::db::message::ToolMeta {
                    tool_calls: Some(calls),
                    tool_call_id: None,
                    tool_name: None,
                },
            )
            .unwrap();
        sessions
            .add_message_full(
                &s.id,
                "tool",
                "结果",
                &crate::db::message::ToolMeta {
                    tool_calls: None,
                    tool_call_id: Some("c1".to_string()),
                    tool_name: Some("send_message".to_string()),
                },
            )
            .unwrap();

        // 正常取（不排除）：历史含全部 5 条。
        let all = crate::core::context::fetch_history(&sessions, &s.id, None, None).unwrap();
        assert_eq!(all.len(), 5);

        // 重试取（排除失败的用户消息 id）：截断到它之前，只剩前两条正常对话——
        // 该消息与它之后的半截轮次都被排除，随后由调用方在末尾显式 push 一次它本身。
        let before =
            crate::core::context::fetch_history(&sessions, &s.id, None, Some(&failed.id)).unwrap();
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].content, "前面的话");
        assert_eq!(before[1].content, "前面的回复");

        // `history_count` 那条取法（宿主显式传条数时走）也必须守住重跑语义：截断发生在
        // 取最近 N 条之后、净化之前，被排除的消息与它后面的半截轮次同样不得入历史。
        let recent =
            crate::core::context::fetch_history(&sessions, &s.id, Some(5), Some(&failed.id))
                .unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[1].content, "前面的回复");
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

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod sessions_tests;
