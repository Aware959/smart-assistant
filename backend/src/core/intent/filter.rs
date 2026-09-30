//! 频率规则过滤：筛出"此刻值得考虑主动开口"的用户（[`filter_eligible`]）。
//!
//! `intent_state` 的**唯一读写方**是 [`crate::core::intent::IntentTracker`]（内存 + 写穿
//! 表）；本模块只保留领域规则——过滤逻辑与候选映射，
//! SQL 层（[`crate::db::intent`]）只负责取行，不做业务判断。

use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::core::intent::IntentCandidate;
use crate::db::intent::IntentRow;

/// 频率规则过滤：返回"当前值得考虑主动开口"的用户。
///
/// 由调用方再按通道可推性（telegram 随时可推 / ilink 需新鲜 context_token）二次筛选。
pub fn filter_eligible(rows: Vec<IntentRow>, now: DateTime<Utc>) -> Vec<IntentCandidate> {
    let cfg = Config::get();
    let min_silence = Duration::hours(cfg.intent_min_silence_hours);
    let max_idle = Duration::days(cfg.intent_max_idle_days);
    let min_gap = Duration::minutes(cfg.intent_min_minutes);

    let today = now.date_naive().to_string();
    rows.into_iter()
        .map(|r| IntentCandidate {
            channel: r.channel,
            external_id: r.external_id,
            session_id: r.session_id,
            last_user_reply_at: r.last_user_reply_at,
            last_intent_at: r.last_intent_at,
            today_count: r.today_count,
        })
        .filter(|c| {
            // 对方至少说过话，且不是正在热聊（静默够了才值得开口）
            let Some(reply_at) = c.last_user_reply_at else {
                return false;
            };
            let quiet = non_negative(now.signed_duration_since(reply_at));
            if quiet < min_silence {
                return false;
            }
            // 太久没回（可能已流失）：不再打扰
            if quiet > max_idle {
                return false;
            }
            // 距上次主动开口够久（防止连环轰炸）
            if let Some(p) = c.last_intent_at {
                if non_negative(now.signed_duration_since(p)) < min_gap {
                    return false;
                }
            }
            // 今日份额：按今日是否主动开口过判断
            let cnt = match c.last_intent_at {
                Some(p) if p.date_naive().to_string() == today => c.today_count,
                _ => 0,
            };
            cnt < cfg.intent_daily_limit as i64
        })
        .collect()
}

/// 时钟回拨等导致的负时长统一按 0 处理。
fn non_negative(d: Duration) -> Duration {
    Duration::max(d, Duration::zero())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::intent;
    use crate::db::session;
    use crate::db::Database;
    use rusqlite::params;

    fn add_user(db: &Database, channel: &str, external_id: &str) -> String {
        let s = session::create(db, "").unwrap();
        let now = Utc::now().to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![channel, external_id, s.id, now, now],
            )
            .unwrap();
        s.id
    }

    #[test]
    fn eligible_respects_silence_and_quota() {
        // 默认配置：min_silence_hours=2, max_idle_days=7, daily_limit=8, min_minutes=45
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "telegram", "100");
        intent::touch_user_reply(&db, "telegram", "100", &sid, Utc::now()).unwrap();

        // 刚说过话 → 静默不足 2h，不应候选
        let now = Utc::now();
        let rows = intent::list_rows(&db).unwrap();
        assert!(filter_eligible(rows, now).is_empty());

        // 假装 3 小时前说过话：把 last_user_reply_at 回拨
        let old = (now - Duration::hours(3)).to_rfc3339();
        db.conn()
            .execute(
                "UPDATE intent_state SET last_user_reply_at = ?1
                 WHERE channel = 'telegram' AND external_id = '100'",
                params![old],
            )
            .unwrap();
        let rows = intent::list_rows(&db).unwrap();
        let el = filter_eligible(rows, now);
        assert_eq!(el.len(), 1);
        assert_eq!(el[0].external_id, "100");

        // 用满配额（today_count=8，且今天主动过）→ 不再候选
        for _ in 0..8 {
            intent::record_intent_send(&db, "telegram", "100", &sid).unwrap();
        }
        let rows = intent::list_rows(&db).unwrap();
        assert!(
            filter_eligible(rows, now).is_empty(),
            "达每日上限后不应再候选"
        );
    }

    #[test]
    fn user_never_spoke_is_not_candidate() {
        let db = Database::in_memory().unwrap();
        add_user(&db, "telegram", "100");
        let rows = intent::list_rows(&db).unwrap();
        assert!(filter_eligible(rows, Utc::now()).is_empty());
    }

    #[test]
    fn cross_day_resets_quota() {
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "ilink", "o123@im.wechat");
        intent::touch_user_reply(&db, "ilink", "o123@im.wechat", &sid, Utc::now()).unwrap();
        let now = Utc::now();
        let old = (now - Duration::hours(3)).to_rfc3339();
        db.conn()
            .execute(
                "UPDATE intent_state SET last_user_reply_at = ?1
                 WHERE channel = 'ilink' AND external_id = 'o123@im.wechat'",
                params![old],
            )
            .unwrap();

        // 全部 8 条发在"昨天"，今天 quota 应重置为可用
        for _ in 0..8 {
            db.conn()
                .execute(
                    "UPDATE intent_state
                     SET today_count = 8,
                         today_date = ?1,
                         last_intent_at = ?2
                     WHERE channel = 'ilink' AND external_id = 'o123@im.wechat'",
                    params![
                        (now - Duration::days(1)).date_naive().to_string(),
                        (now - Duration::days(1)).to_rfc3339()
                    ],
                )
                .unwrap();
        }
        let rows = intent::list_rows(&db).unwrap();
        assert_eq!(filter_eligible(rows, now).len(), 1, "跨天后配额应重置");
    }
}
