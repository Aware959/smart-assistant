//! 主动推送的频率与状态簿：记录每个外部用户（channel × external_id）的
//! 最近互动时间、最近主动发送时间、每日主动上限，供主动陪伴引擎做"像真人"的频率控制。

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;
use crate::config::Config;

/// 全部候选：有会话映射的外部用户（left join 状态，未建状态的行以默认值出现）。
#[derive(Debug, Clone)]
pub struct ProactiveCandidate {
    pub channel: String,
    pub external_id: String,
    pub session_id: String,
    /// 用户最后一次入站消息时间（RFC3339）；从没记录过则为 None。
    pub last_user_reply_at: Option<DateTime<Utc>>,
    /// 上次主动发送时间；从未主动发过则为 None。
    pub last_proactive_at: Option<DateTime<Utc>>,
    /// 今日已主动发送条数。
    pub today_count: i64,
}

/// 保证 (channel, external_id) 在状态表有行，返回该行（read 到当前值）。
pub fn ensure_state(db: &Database, channel: &str, external_id: &str, session_id: &str) -> Result<()> {
    db.conn().execute(
        "INSERT OR IGNORE INTO proactive_state
             (channel, external_id, session_id)
         VALUES (?1, ?2, ?3)",
        params![channel, external_id, session_id],
    )?;
    Ok(())
}

/// 用户发来消息时调用：记录"对方刚说话"，并刷新 session 指向。
pub fn touch_user_reply(
    db: &Database,
    channel: &str,
    external_id: &str,
    session_id: &str,
) -> Result<()> {
    ensure_state(db, channel, external_id, session_id)?;
    let now = Utc::now().to_rfc3339();
    db.conn().execute(
        "UPDATE proactive_state
         SET last_user_reply_at = ?1, session_id = ?2
         WHERE channel = ?3 AND external_id = ?4",
        params![now, session_id, channel, external_id],
    )?;
    Ok(())
}

/// 主动发送成功后调用：更新最近主动时间与今日计数（跨天自动清零）。
pub fn record_proactive_send(
    db: &Database,
    channel: &str,
    external_id: &str,
    session_id: &str,
) -> Result<()> {
    ensure_state(db, channel, external_id, session_id)?;
    let now = Utc::now();
    let today = now.date_naive().to_string();
    let now_rfc = now.to_rfc3339();

    db.conn().execute(
        "UPDATE proactive_state
         SET last_proactive_at = ?1,
             today_count = CASE WHEN today_date = ?2 THEN today_count + 1 ELSE 1 END,
             today_date = ?3
         WHERE channel = ?4 AND external_id = ?5",
        params![now_rfc, today, today, channel, external_id],
    )?;
    Ok(())
}

/// 读取某用户最近一次入站回复时间（从未记录则为 None）。
pub fn last_user_reply_at(
    db: &Database,
    channel: &str,
    external_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT last_user_reply_at FROM proactive_state
         WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |row| {
        row.get::<_, Option<String>>(0)
    })?;
    match rows.next() {
        Some(Ok(v)) => Ok(parse_ts(v)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

fn parse_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// 读取某用户最近一次主动外发时间（从未主动发过则为 None）。
pub fn last_proactive_at(
    db: &Database,
    channel: &str,
    external_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT last_proactive_at FROM proactive_state
         WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |row| {
        row.get::<_, Option<String>>(0)
    })?;
    match rows.next() {
        Some(Ok(v)) => Ok(parse_ts(v)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// 列出全部候选用户（有会话映射即候选），未建状态的行以默认统计出现。
pub fn list_candidates(db: &Database) -> Result<Vec<ProactiveCandidate>> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT
                cs.channel,
                cs.external_id,
                cs.session_id,
                ps.last_user_reply_at,
                ps.last_proactive_at,
                COALESCE(ps.today_count, 0)
             FROM channel_sessions cs
             LEFT JOIN proactive_state ps
               ON ps.channel = cs.channel AND ps.external_id = cs.external_id",
        )?;
    let rows = stmt.query_map([], |row| {
        Ok(ProactiveCandidate {
            channel: row.get(0)?,
            external_id: row.get(1)?,
            session_id: row.get(2)?,
            last_user_reply_at: parse_ts(row.get(3)?),
            last_proactive_at: parse_ts(row.get(4)?),
            today_count: row.get(5)?,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 频率规则过滤：返回"当前值得考虑主动联系"的用户。
///
/// 由调用方再按通道可推性（telegram 随时可推 / ilink 需新鲜 context_token）二次筛选。
pub fn eligible_candidates(db: &Database, now: DateTime<Utc>) -> Result<Vec<ProactiveCandidate>> {
    let cfg = Config::get();
    let min_silence = chrono::Duration::hours(cfg.proactive_min_silence_hours as i64);
    let max_idle = chrono::Duration::days(cfg.proactive_max_idle_days as i64);
    let min_gap = chrono::Duration::minutes(cfg.proactive_min_minutes as i64);

    let today = now.date_naive().to_string();
    // 跨天时今日计数已过期，视为 0
    Ok(list_candidates(db)?
        .into_iter()
        .filter(|c| {
            // 对方至少说过话，且不是正在热聊（静默够了才值得主动）
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
            // 距上次主动发送够久（防止连环轰炸）
            if let Some(p) = c.last_proactive_at {
                if non_negative(now.signed_duration_since(p)) < min_gap {
                    return false;
                }
            }
            // 今日份额：按 today_date 判断是否计入今日
            let cnt = if is_same_today(c, &today) {
                c.today_count
            } else {
                0
            };
            cnt < cfg.proactive_daily_limit as i64
        })
        .collect())
}

/// 时钟回拨等导致的负时长统一按 0 处理。
fn non_negative(d: chrono::Duration) -> chrono::Duration {
    chrono::Duration::max(d, chrono::Duration::zero())
}

fn is_same_today(c: &ProactiveCandidate, today: &str) -> bool {
    // today_count > 0 时对应的 today_date 无法从候选读取（查询未带该列）；
    // 采用保守策略：只有最近一次主动发送发生在今日才计入今日配额。
    match c.last_proactive_at {
        Some(p) => p.date_naive().to_string() == today,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::db::session;

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
    fn touch_and_record_roundtrip() {
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "telegram", "100");
        touch_user_reply(&db, "telegram", "100", &sid).unwrap();

        let cands = list_candidates(&db).unwrap();
        assert_eq!(cands.len(), 1);
        assert!(cands[0].last_user_reply_at.is_some());
        assert_eq!(cands[0].today_count, 0);

        record_proactive_send(&db, "telegram", "100", &sid).unwrap();
        record_proactive_send(&db, "telegram", "100", &sid).unwrap();
        let cands = list_candidates(&db).unwrap();
        assert_eq!(cands[0].today_count, 2);
        assert!(cands[0].last_proactive_at.is_some());
    }

    #[test]
    fn eligible_respects_silence_and_quota() {
        let db = Database::in_memory().unwrap();
        // 默认配置：min_silence_hours=2, max_idle_days=7, daily_limit=8, min_minutes=45
        let sid = add_user(&db, "telegram", "100");
        touch_user_reply(&db, "telegram", "100", &sid).unwrap();

        // 刚说过话 → 静默不足 2h，不应候选
        let now = Utc::now();
        let el = eligible_candidates(&db, now).unwrap();
        assert!(el.is_empty());

        // 假装 3 小时前说过话：把 last_user_reply_at 回拨
        let old = (now - chrono::Duration::hours(3)).to_rfc3339();
        db.conn()
            .execute(
                "UPDATE proactive_state SET last_user_reply_at = ?1
                 WHERE channel = 'telegram' AND external_id = '100'",
                params![old],
            )
            .unwrap();
        let el = eligible_candidates(&db, now).unwrap();
        assert_eq!(el.len(), 1);
        assert_eq!(el[0].external_id, "100");

        // 用满配额（today_count=8，且今天主动过）→ 不再候选
        for _ in 0..8 {
            record_proactive_send(&db, "telegram", "100", &sid).unwrap();
        }
        let el = eligible_candidates(&db, now).unwrap();
        assert!(el.is_empty(), "达每日上限后不应再候选");
    }

    #[test]
    fn user_never_spoke_is_not_candidate() {
        let db = Database::in_memory().unwrap();
        add_user(&db, "telegram", "100");
        // 从未 touch_user_reply
        let el = eligible_candidates(&db, Utc::now()).unwrap();
        assert!(el.is_empty());
    }

    #[test]
    fn cross_day_resets_quota() {
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "ilink", "o123@im.wechat");
        touch_user_reply(&db, "ilink", "o123@im.wechat", &sid).unwrap();
        let now = Utc::now();
        let old = (now - chrono::Duration::hours(3)).to_rfc3339();
        db.conn()
            .execute(
                "UPDATE proactive_state SET last_user_reply_at = ?1
                 WHERE channel = 'ilink' AND external_id = 'o123@im.wechat'",
                params![old],
            )
            .unwrap();

        // 全部 8 条发在"昨天"，今天 quota 应重置为可用
        for _ in 0..8 {
            db.conn()
                .execute(
                    "UPDATE proactive_state
                     SET today_count = 8,
                         today_date = ?1,
                         last_proactive_at = ?2
                     WHERE channel = 'ilink' AND external_id = 'o123@im.wechat'",
                    params![
                        (now - chrono::Duration::days(1))
                            .date_naive()
                            .to_string(),
                        (now - chrono::Duration::days(1)).to_rfc3339()
                    ],
                )
                .unwrap();
        }
        let el = eligible_candidates(&db, now).unwrap();
        assert_eq!(el.len(), 1, "跨天后配额应重置");
    }
}