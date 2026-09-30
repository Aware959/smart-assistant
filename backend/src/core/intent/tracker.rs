//! 意图状态追踪器：`intent_state` 的唯一读写方（内存镜像 + 写穿）。
//!
//! 启动时把候选行全量水合进内存，入站 / 外发记录写穿 `intent_state` 表后同步
//! 内存；频率规则过滤（静默时长 / 流失 / 间隔 / 每日配额）复用
//! [`crate::core::intent::filter::filter_eligible`]。core 内以具体类型直接接线。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::core::intent::filter;
use crate::core::intent::IntentCandidate;
use crate::db::intent::IntentRow;
use crate::db::{lock_db, Database};
use crate::error::Result;
use crate::utils::time;

type Key = (String, String);

/// 一位候选用户的意图状态（内存驻留，进程存活期为值）。
#[derive(Debug, Clone)]
struct RowState {
    channel: String,
    external_id: String,
    session_id: String,
    last_user_reply_at: Option<DateTime<Utc>>,
    last_intent_at: Option<DateTime<Utc>>,
    /// 今日已主动发送条数。
    today_count: i64,
    /// 配额归属的本地日期（yyyy-mm-dd）；非今天则计数视为 0。
    today_date: String,
}

impl RowState {
    fn from_row(r: IntentRow) -> Self {
        let today = time::local_date();
        // 水合时没有"配额归属日期"栏位：按最近意图开口时刻是否在今天就近还原。
        let today_date = match r.last_intent_at {
            Some(p) if p.date_naive().to_string() == today => today.clone(),
            _ => String::new(),
        };
        Self {
            channel: r.channel,
            external_id: r.external_id,
            session_id: r.session_id,
            last_user_reply_at: r.last_user_reply_at,
            last_intent_at: r.last_intent_at,
            today_count: r.today_count,
            today_date,
        }
    }

    fn to_row(&self) -> IntentRow {
        IntentRow {
            channel: self.channel.clone(),
            external_id: self.external_id.clone(),
            session_id: self.session_id.clone(),
            last_user_reply_at: self.last_user_reply_at,
            last_intent_at: self.last_intent_at,
            today_count: self.today_count,
        }
    }
}

#[derive(Default)]
struct Inner {
    rows: HashMap<Key, RowState>,
}

/// 意图状态的内存适配器（由组装层 [`crate::core::app`] 装配，组件直接持引用调用）。
#[derive(Clone)]
pub struct IntentTracker {
    db: Arc<Mutex<Database>>,
    inner: Arc<Mutex<Inner>>,
}

impl IntentTracker {
    pub fn new(db: Arc<Mutex<Database>>) -> Result<Self> {
        let rows = crate::db::intent::list_rows(&lock_db(&db))?
            .into_iter()
            .map(|r| {
                let key = (r.channel.clone(), r.external_id.clone());
                (key, RowState::from_row(r))
            })
            .collect();
        Ok(Self {
            db,
            inner: Arc::new(Mutex::new(Inner { rows })),
        })
    }

    fn lock(&self) -> impl std::ops::DerefMut<Target = Inner> + '_ {
        self.inner.lock().unwrap()
    }

    /// 某用户最近一次意图开口（主动外发）时间（从未主动发过则为 None）——供世界快照组装。
    pub fn last_intent_at(
        &self,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .lock()
            .rows
            .get(&(channel.to_string(), external_id.to_string()))
            .and_then(|st| st.last_intent_at))
    }
}

impl IntentTracker {
    /// 记录"对方刚发消息"（确保状态行存在并刷新最近入站时刻）。
    ///
    /// `at` 传**消息真实发出时刻**（通道报文时间戳），缺省才用接收时刻。
    /// 内存与 DB 两侧都只前进不后退，与 SQL 层的单调约束保持一致。
    pub fn touch_user_reply(
        &self,
        channel: &str,
        external_id: &str,
        session_id: &str,
        at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let at = at.unwrap_or_else(time::now);
        crate::db::intent::touch_user_reply(
            &lock_db(&self.db),
            channel,
            external_id,
            session_id,
            at,
        )?;
        let mut inner = self.lock();
        let st = inner
            .rows
            .entry((channel.to_string(), external_id.to_string()))
            .or_insert_with(|| RowState {
                channel: channel.to_string(),
                external_id: external_id.to_string(),
                session_id: session_id.to_string(),
                last_user_reply_at: None,
                last_intent_at: None,
                today_count: 0,
                today_date: String::new(),
            });
        if st.last_user_reply_at.is_none_or(|prev| prev < at) {
            st.last_user_reply_at = Some(at);
        }
        st.session_id = session_id.to_string();
        Ok(())
    }

    /// 记录一次意图开口（主动发送）成功（刷新最近开口时刻与今日计数，跨天自动清零）。
    pub fn record_intent_send(
        &self,
        channel: &str,
        external_id: &str,
        session_id: &str,
    ) -> Result<()> {
        crate::db::intent::record_intent_send(&lock_db(&self.db), channel, external_id, session_id)?;
        let now = time::now();
        let today = time::local_date();
        let mut inner = self.lock();
        let st = inner
            .rows
            .entry((channel.to_string(), external_id.to_string()))
            .or_insert_with(|| RowState {
                channel: channel.to_string(),
                external_id: external_id.to_string(),
                session_id: session_id.to_string(),
                last_user_reply_at: None,
                last_intent_at: None,
                today_count: 0,
                today_date: String::new(),
            });
        st.last_intent_at = Some(now);
        st.today_count = if st.today_date == today { st.today_count + 1 } else { 1 };
        st.today_date = today;
        Ok(())
    }

    /// 某用户最近一次入站回复时间（从未记录则为 None）。
    pub fn last_user_reply_at(
        &self,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .lock()
            .rows
            .get(&(channel.to_string(), external_id.to_string()))
            .and_then(|st| st.last_user_reply_at))
    }

    /// 频率规则过滤后的候选列表（静默时长 / 流失 / 间隔 / 每日配额）。
    pub fn eligible_candidates(&self, now: DateTime<Utc>) -> Result<Vec<IntentCandidate>> {
        let rows: Vec<IntentRow> = self
            .lock()
            .rows
            .values()
            .map(RowState::to_row)
            .collect();
        Ok(filter::filter_eligible(rows, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use crate::db::{session, Database};
    use rusqlite::params;

    fn tracker_with_user(channel: &str, external: &str) -> (Arc<Mutex<Database>>, IntentTracker, String) {
        let db = Arc::new(Mutex::new(Database::in_memory().unwrap()));
        // 建渠道映射（意图候选来自 channel_sessions）。
        let sid = {
            let g = lock_db(&db);
            let s = session::create(&g, "").unwrap();
            g.conn()
                .execute(
                    "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![channel, external, s.id, Utc::now().to_rfc3339()],
                )
                .unwrap();
            s.id
        };
        let tracker = IntentTracker::new(db.clone()).unwrap();
        (db, tracker, sid)
    }

    #[test]
    fn touch_and_record_sync_memory_and_db() {
        let (db, tracker, sid) = tracker_with_user("telegram", "100");
        tracker
            .touch_user_reply("telegram", "100", &sid, Some(Utc::now()))
            .unwrap();
        assert!(
            tracker.last_user_reply_at("telegram", "100").unwrap().is_some(),
            "touch 后内存即有最近入站时刻"
        );

        tracker.record_intent_send("telegram", "100", &sid).unwrap();
        tracker.record_intent_send("telegram", "100", &sid).unwrap();
        assert_eq!(
            tracker.lock().rows.get(&("telegram".into(), "100".into())).unwrap().today_count,
            2
        );

        // 写穿验证：DB 行一致。
        let g = lock_db(&db);
        let rows = crate::db::intent::list_rows(&g).unwrap();
        assert_eq!(rows[0].today_count, 2);
    }

    #[test]
    fn fresh_book_hydrates_from_db() {
        let (db, tracker, sid) = tracker_with_user("telegram", "100");
        tracker
            .touch_user_reply("telegram", "100", &sid, Some(Utc::now()))
            .unwrap();
        crate::db::intent::record_intent_send(&lock_db(&db), "telegram", "100", &sid).unwrap();

        // 新进程视角：新建运行簿从 DB 水合。
        let fresh = IntentTracker::new(db).unwrap();
        assert!(
            fresh.last_user_reply_at("telegram", "100").unwrap().is_some(),
            "水合后 last_user_reply_at 可见"
        );
    }

    #[test]
    fn eligible_reads_from_memory() {
        let (_, tracker, sid) = tracker_with_user("telegram", "100");
        tracker
            .touch_user_reply("telegram", "100", &sid, Some(Utc::now()))
            .unwrap();
        let now = Utc::now();
        // 刚说过话 → 静默不足 2h，不应候选。
        assert!(tracker.eligible_candidates(now).unwrap().is_empty());

        // 回拨内存里的最近入站时刻到 3 小时前（模拟时钟推进）。
        let mut inner = tracker.lock();
        let st = inner.rows.get_mut(&("telegram".into(), "100".into())).unwrap();
        st.last_user_reply_at = Some(now - Duration::hours(3));
        drop(inner);
        let el = tracker.eligible_candidates(now).unwrap();
        assert_eq!(el.len(), 1);
        assert_eq!(el[0].external_id, "100");
    }

    /// 离线 5 小时后补收的消息，内存与 DB 都应记**真实发出时刻**。
    ///
    /// 这正是修复前的 bug：用接收时刻会让静默窗口从 0 起算（5h → 0），
    /// 引擎误以为对方刚说过话而**压掉**本该发生的主动开口。记真实时刻后
    /// 窗口是 5h，越过 2h 门槛 → 成为候选。
    #[test]
    fn late_delivered_message_uses_real_age_not_receive_time() {
        let (db, tracker, sid) = tracker_with_user("telegram", "100");
        let sent_at = Utc::now() - Duration::hours(5);
        tracker
            .touch_user_reply("telegram", "100", &sid, Some(sent_at))
            .unwrap();

        let now = Utc::now();
        assert_eq!(
            tracker.last_user_reply_at("telegram", "100").unwrap(),
            Some(sent_at),
            "内存应记真实发出时刻"
        );
        let rows = crate::db::intent::list_rows(&lock_db(&db)).unwrap();
        assert_eq!(rows[0].last_user_reply_at, Some(sent_at), "DB 应同步真实时刻");
        assert_eq!(
            tracker.eligible_candidates(now).unwrap().len(),
            1,
            "静默 5h 已越过 2h 门槛，应为候选（修复前因窗口归零被误压掉）"
        );
    }

    /// 对照：消息确实是刚收到的（1 分钟前），静默不足 2h，不该是候选。
    /// 与上一条成对，共同锁住"窗口按真实时差计算"这一语义。
    #[test]
    fn just_received_message_is_not_yet_eligible() {
        let (_, tracker, sid) = tracker_with_user("telegram", "100");
        tracker
            .touch_user_reply("telegram", "100", &sid, Some(Utc::now()))
            .unwrap();
        assert!(
            tracker.eligible_candidates(Utc::now()).unwrap().is_empty(),
            "静默 0 分钟，不足 2h，不该是候选"
        );
    }

    /// 迟到的旧消息不应把时刻拉回过去（内存侧与 db 侧同规则）。
    #[test]
    fn late_delivery_never_moves_timestamp_backwards() {
        let (_, tracker, sid) = tracker_with_user("telegram", "100");
        let recent = Utc::now();
        tracker
            .touch_user_reply("telegram", "100", &sid, Some(recent))
            .unwrap();
        tracker
            .touch_user_reply(
                "telegram",
                "100",
                &sid,
                Some(recent - Duration::hours(3)),
            )
            .unwrap();
        assert_eq!(
            tracker.last_user_reply_at("telegram", "100").unwrap(),
            Some(recent)
        );
    }
}