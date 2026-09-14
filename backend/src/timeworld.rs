//! 时间世界模型：让 AI 拥有"进行时"感知，而不只是被动应答。
//!
//! AI 需要一份自己的世界状态——此刻几点、距对方最后说话过了多久、对方大致什么作息、
//! 上次主动联系距今多久——并把它注入回复与主动决策的提示词，使 AI 能自然地
//! 引用时间、把握节奏，像真人一样活在流动的时间里。
//!
//! `messages.created_at` 现在携带的是通道报文里的真实用户时刻（见 [`crate::channels`]），
//! 这份时间线是画像推断的唯一依据（`observe` 增量重算、`snapshot` 只读组装）。

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;

// ---------- 时段 ----------

/// 一天里的阶段（按某时区的钟表小时划分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    DeepNight,
    EarlyMorning,
    Morning,
    Forenoon,
    Noon,
    Afternoon,
    Evening,
    Night,
}

/// 0-23 小时 → 时段。
pub fn phase_of(hour: u32) -> Phase {
    match hour {
        0..=4 => Phase::DeepNight,
        5..=6 => Phase::EarlyMorning,
        7..=9 => Phase::Morning,
        10..=11 => Phase::Forenoon,
        12..=13 => Phase::Noon,
        14..=17 => Phase::Afternoon,
        18..=19 => Phase::Evening,
        _ => Phase::Night,
    }
}

pub fn phase_label(p: Phase) -> &'static str {
    match p {
        Phase::DeepNight => "深夜",
        Phase::EarlyMorning => "清晨",
        Phase::Morning => "早上",
        Phase::Forenoon => "上午",
        Phase::Noon => "中午",
        Phase::Afternoon => "下午",
        Phase::Evening => "傍晚",
        Phase::Night => "晚上",
    }
}

// ---------- 快照 ----------

/// 一次"此刻"的世界状态（从 DB 只读组装，不落盘）。
#[derive(Debug, Default, Clone)]
pub struct TimeWorld {
    pub now: DateTime<Utc>,
    /// 对方最后一次入站消息的时刻（通道真实时刻；无记录则 None）。
    pub last_user_at: Option<DateTime<Utc>>,
    /// 上次主动联系的时刻。
    pub last_proactive_at: Option<DateTime<Utc>>,
    /// 推断的对方时区偏移（分钟，东为正值）。
    pub user_offset_minutes: Option<i32>,
    /// 对方历史最活跃小时（UTC，画像众数）。
    pub user_active_hour: Option<u32>,
    /// 世界引擎的自我状态：生活设定 + 今日叙事 + 情绪（见 [`crate::world`]）。
    pub ai_base: String,
    pub ai_day: Option<String>,
    pub ai_mood: Option<crate::world::emotion::Mood>,
    /// 与对方的关系（无记录为 None）。
    pub relation: Option<crate::world::relation::Relation>,
}

/// 从真实 DB 状态组装一次世界快照（无渠道映射的 CLI/HTTP 会话也可用消息时间线兜底）。
pub fn snapshot(db: &Database, session_id: &str) -> TimeWorld {
    let mut w = TimeWorld {
        now: Utc::now(),
        ..Default::default()
    };
    if let Ok(Some((channel, external))) = channel_of(db, session_id) {
        if let Ok(Some(t)) = crate::db::proactive::last_user_reply_at(db, &channel, &external) {
            w.last_user_at = Some(t);
        }
        if let Ok(Some(p)) = last_proactive_at(db, &channel, &external) {
            w.last_proactive_at = Some(p);
        }
        if let Ok(Some(profile)) = profile_of(db, &channel, &external) {
            w.user_offset_minutes = Some(profile.0);
            w.user_active_hour = Some(profile.1);
        }
        if let Ok(Some(r)) = crate::world::relation::load(db, &channel, &external) {
            w.relation = Some(r);
        }
    }
    if w.last_user_at.is_none() {
        if let Ok(Some(t)) = latest_user_msg_at(db, session_id) {
            w.last_user_at = Some(t);
        }
    }
    if let Ok(st) = crate::world::self_state::load(db) {
        w.ai_base = st.self_base;
        w.ai_day = (!st.today_narrative.trim().is_empty()).then_some(st.today_narrative);
        w.ai_mood = Some(st.mood);
    }
    w
}

/// 把快照渲染成提示词里的一段"此刻的世界"（空快照也至少给出 AI 本机时间）。
pub fn render(w: &TimeWorld) -> String {
    let local = Local::now();
    let mut s = String::from("【此刻的世界】");
    s.push_str(&format!(
        "\n- AI 本机时间：{}，{}。",
        fmt_local_dt(local),
        phase_label(phase_of(local.hour()))
    ));
    if let Some(u) = w.last_user_at {
        s.push_str(&format!(
            "\n- 对方最后发言：{}（{}前）。",
            fmt_local_dt(u.with_timezone(&Local)),
            fmt_ago(u, w.now)
        ));
    }
    if let Some(off) = w.user_offset_minutes {
        let user_local = w.now + chrono::Duration::minutes(off as i64);
        s.push_str(&format!(
            "\n- 推断对方当地现在是：{}，{}。",
            fmt_hm(user_local),
            phase_label(phase_of(user_local.hour()))
        ));
        if let Some(ah) = w.user_active_hour {
            let local_ah = (ah as i32 + off.div_euclid(60)).rem_euclid(24) as u32;
            s.push_str(&format!(
                "\n- 对方作息：通常在{}前后这个时段活跃。",
                phase_label(phase_of(local_ah))
            ));
        }
    }
    if let Some(p) = w.last_proactive_at {
        s.push_str(&format!("\n- 距上次主动联系：{}前。", fmt_ago(p, w.now)));
    }
    if !w.ai_base.trim().is_empty() {
        s.push_str(&format!("\n- 我的生活设定：{}", w.ai_base.trim()));
    }
    if let Some(m) = &w.ai_mood {
        s.push_str(&format!("\n- 此刻的心境：{}。", crate::world::emotion::label(m)));
    }
    if let Some(day) = &w.ai_day {
        s.push_str(&format!("\n- 今天到现在我经历了：{day}"));
    }
    if let Some(r) = &w.relation {
        s.push_str(&format!(
            "\n- 我和对方的关系：{}，{}。",
            crate::world::relation::closeness_label(r.closeness),
            crate::world::relation::trust_label(r.trust)
        ));
    }
    s
}

// ---------- 画像落盘 ----------

type Profile = (i32, u32); // (offset_minutes, active_hour_utc)

/// 真实时刻的用户消息落库后调用：重算该用户作息画像并 upsert。
/// 没有渠道映射（CLI/HTTP 会话）时静默跳过。
pub fn observe(db: &Database, session_id: &str) -> Result<()> {
    let Some((channel, external)) = channel_of(db, session_id)? else {
        return Ok(());
    };
    let Some((off, active_hour, n)) = infer_profile(db, &channel, &external)? else {
        return Ok(());
    };
    db.conn().execute(
        "INSERT INTO time_profiles (channel, external_id, utc_offset_minutes, active_hour, observations, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(channel, external_id) DO UPDATE SET
           utc_offset_minutes = excluded.utc_offset_minutes,
           active_hour       = excluded.active_hour,
           observations      = excluded.observations,
           updated_at        = excluded.updated_at",
        params![
            channel,
            external,
            off,
            active_hour as i64,
            n,
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

fn profile_of(db: &Database, channel: &str, external: &str) -> Result<Option<Profile>> {
    let mut stmt = db.conn().prepare(
        "SELECT utc_offset_minutes, active_hour, observations
         FROM time_profiles WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external], |r| {
        Ok((
            r.get::<_, Option<i32>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    match rows.next() {
        Some(Ok((off, ah, n))) if n > 0 => {
            Ok(off.and_then(|o| ah.map(|a| (o, a as u32))))
        }
        Some(Ok(_)) => Ok(None),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// 从该用户全部 user 消息的时刻推断作息：最活跃小时（众数）+ 时区偏移。
///
/// 时区推断假设"对方当地活跃窗口通常是 9:00~23:59"，对每个候选偏移统计该窗口内
/// 的消息占比，取占比最高的偏移；同分偏好东八区（更稳的默认可协商）。
fn infer_profile(db: &Database, channel: &str, external: &str) -> Result<Option<(i32, u32, i64)>> {
    let mut stmt = db.conn().prepare(
        "SELECT m.created_at
         FROM messages m
         JOIN channel_sessions cs ON cs.session_id = m.session_id
         WHERE cs.channel = ?1 AND cs.external_id = ?2 AND m.role = 'user'",
    )?;
    let hours = stmt
        .query_map(params![channel, external], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|ts| DateTime::parse_from_rfc3339(&ts).ok())
        .map(|dt| dt.with_timezone(&Utc).hour())
        .collect::<Vec<u32>>();
    if hours.is_empty() {
        return Ok(None);
    }

    let mut counts = HashMap::<u32, usize>::new();
    for &h in &hours {
        *counts.entry(h).or_default() += 1;
    }
    let active_hour = *counts
        .iter()
        .max_by_key(|(h, c)| (**c, 24 - **h))
        .map(|(h, _)| h)
        .unwrap_or(&0);

    let mut best: Option<(i32, usize)> = None;
    for off_h in -12..=14 {
        let ok = hours
            .iter()
            .filter(|h| {
                let lh = (**h as i32 + off_h).rem_euclid(24);
                (9..24).contains(&lh)
            })
            .count();
        let better = match best {
            None => true,
            Some((bo, bc)) => ok > bc || (ok == bc && prefer(&off_h, &bo)),
        };
        if better {
            best = Some((off_h, ok));
        }
    }
    let offset_minutes = best.map(|(off_h, _)| off_h * 60).unwrap_or(8 * 60);
    Ok(Some((offset_minutes, active_hour, hours.len() as i64)))
}

/// 同分时偏好东八区，其次偏移绝对值小。
fn prefer(a: &i32, b: &i32) -> bool {
    let da = (8 - a).unsigned_abs();
    let db = (8 - b).unsigned_abs();
    da < db || (da == db && a.unsigned_abs() < b.unsigned_abs())
}

// ---------- DB 读取 ----------

pub(crate) fn channel_of(db: &Database, session_id: &str) -> Result<Option<(String, String)>> {
    let mut stmt = db.conn().prepare(
        "SELECT channel, external_id
         FROM channel_sessions WHERE session_id = ?1 LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    match rows.next() {
        Some(Ok(v)) => Ok(Some(v)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

fn last_proactive_at(
    db: &Database,
    channel: &str,
    external: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT last_proactive_at FROM proactive_state
         WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external], |r| r.get::<_, Option<String>>(0))?;
    match rows.next() {
        Some(Ok(v)) => Ok(parse_ts(v)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

fn latest_user_msg_at(db: &Database, session_id: &str) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = db.conn().prepare(
        "SELECT created_at FROM messages
         WHERE session_id = ?1 AND role = 'user'
         ORDER BY created_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
    match rows.next() {
        Some(Ok(v)) => Ok(parse_ts(Some(v))),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

fn parse_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|d| d.with_timezone(&Utc))
}

// ---------- 格式化 ----------

const WEEKDAYS: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

fn fmt_local_dt(t: DateTime<Local>) -> String {
    format!(
        "{}月{}日 {} {}",
        t.month(),
        t.day(),
        WEEKDAYS[t.weekday().num_days_from_monday() as usize],
        fmt_hm(t)
    )
}

fn fmt_hm(t: impl Timelike) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// "x 分钟 / x 小时 y 分 / x 天前"的间隔描述；未来时刻按 0 秒处理。
pub fn fmt_ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(t).num_seconds().max(0);
    if secs < 60 {
        format!("{secs} 秒")
    } else if secs < 3600 {
        format!("{} 分钟", secs / 60)
    } else if secs < 86400 {
        format!("{} 小时 {}", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{} 天", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::session;
    use crate::db::Database;

    fn add_user(db: &Database, channel: &str, external: &str) -> String {
        let s = session::create(db, "").unwrap();
        let now = Utc::now().to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![channel, external, s.id, now, now],
            )
            .unwrap();
        s.id
    }

    fn push_user_msg(db: &Database, session: &str, ts: &str) {
        db.conn()
            .execute(
                "INSERT INTO messages (id, session_id, role, content, created_at)
                 VALUES (?1, ?2, 'user', 'x', ?3)",
                params![uuid::Uuid::new_v4().to_string(), session, ts],
            )
            .unwrap();
    }

    #[test]
    fn phase_boundaries() {
        assert_eq!(phase_of(0), Phase::DeepNight);
        assert_eq!(phase_of(4), Phase::DeepNight);
        assert_eq!(phase_of(5), Phase::EarlyMorning);
        assert_eq!(phase_of(7), Phase::Morning);
        assert_eq!(phase_of(11), Phase::Forenoon);
        assert_eq!(phase_of(13), Phase::Noon);
        assert_eq!(phase_of(17), Phase::Afternoon);
        assert_eq!(phase_of(19), Phase::Evening);
        assert_eq!(phase_of(23), Phase::Night);
        assert!(!phase_label(Phase::DeepNight).is_empty());
    }

    #[test]
    fn infers_east8_offset_and_active_hour() {
        // 我们的北京用户：UTC 3 点/4 点发消息 = 东八区 11/12 点（上午活跃）。
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "ilink", "o@im.wechat");
        push_user_msg(&db, &sid, "2026-09-14T03:47:00+00:00");
        push_user_msg(&db, &sid, "2026-09-13T04:10:00+00:00");
        push_user_msg(&db, &sid, "2026-09-12T03:00:00+00:00");

        let p = infer_profile(&db, "ilink", "o@im.wechat").unwrap().unwrap();
        assert_eq!(p.0, 8 * 60, "应推断出东八区");
        assert_eq!(p.1, 3, "UTC 3 点为最活跃小时");
        assert_eq!(p.2, 3);
    }

    #[test]
    fn observe_upserts_profile() {
        let db = Database::in_memory().unwrap();
        let sid = add_user(&db, "telegram", "100");
        push_user_msg(&db, &sid, "2026-09-14T03:47:00+00:00");
        observe(&db, &sid).unwrap();

        let got = profile_of(&db, "telegram", "100").unwrap().unwrap();
        assert_eq!(got.0, 8 * 60);

        push_user_msg(&db, &sid, "2026-09-14T13:00:00+00:00");
        observe(&db, &sid).unwrap();
        let prof2 = profile_of(&db, "telegram", "100").unwrap().unwrap();
        assert_eq!(prof2.0, 8 * 60, "画像应保持推断的时区");
    }

    #[test]
    fn observing_without_channel_is_noop() {
        let db = Database::in_memory().unwrap();
        let s = session::create(&db, "").unwrap();
        observe(&db, &s.id).unwrap(); // 无渠道映射，不报错
    }

    #[test]
    fn ago_formatting() {
        let now = Utc::now();
        let base = now - chrono::Duration::seconds(30);
        assert!(fmt_ago(base, now).contains('秒'));
        let m = now - chrono::Duration::minutes(5);
        assert!(fmt_ago(m, now).contains("5 分钟"));
        let h = now - chrono::Duration::hours(2);
        assert!(fmt_ago(h, now).contains("2 小时"));
        let d = now - chrono::Duration::days(3);
        assert!(fmt_ago(d, now).contains("3 天"));
    }
}