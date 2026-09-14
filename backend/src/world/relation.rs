//! 关系（按用户）：亲密度 × 信任度。交互会升温，久不往来会随时间衰减，
//! 与 [`crate::world::emotion`] 一样是"有遗忘、有冷却"的慢变量。
//!
//! 属于加分类数值模型：每次对方发来消息小步升温，同时随时间指数降温。
//! 更精细的情感评估（消息情绪/内容深浅）留待后续迭代，结构先行。

use chrono::{DateTime, Duration, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;

/// 首次认识时的初始亲密度 / 信任度。
pub const START_CLOSENESS: f32 = 0.15;
pub const START_TRUST: f32 = 0.05;
/// 来一条消息的升温量（封顶 1.0）。
pub const BUMP_CLOSENESS: f32 = 0.03;
pub const BUMP_TRUST: f32 = 0.01;
/// 冷场的半衰期（天）。
const CLOSENESS_HALF_LIFE_DAYS: f64 = 14.0;
const TRUST_HALF_LIFE_DAYS: f64 = 40.0;

#[derive(Debug, Clone)]
pub struct Relation {
    pub channel: String,
    pub external_id: String,
    pub closeness: f32,
    pub trust: f32,
    pub updated_at: DateTime<Utc>,
}

impl Relation {
    /// 过期衰减：按距上次更新的天数指数降温到今天。
    pub fn decayed(&self, now: DateTime<Utc>) -> Relation {
        let days = now
            .signed_duration_since(self.updated_at.max(now - Duration::days(365)))
            .num_milliseconds() as f64
            / 86_400_000.0;
        Relation {
            channel: self.channel.clone(),
            external_id: self.external_id.clone(),
            closeness: self.closeness * decay_factor(days, CLOSENESS_HALF_LIFE_DAYS),
            trust: self.trust * decay_factor(days, TRUST_HALF_LIFE_DAYS),
            updated_at: now,
        }
    }
}

/// 单次用户消息后的关系升温（无渠道映射的会话静默跳过）。
pub fn observe(db: &Database, session_id: &str) -> Result<()> {
    let Some((channel, external)) = crate::timeworld::channel_of(db, session_id)? else {
        return Ok(());
    };
    observe_channel(db, &channel, &external)
}

/// 对某位用户升温。没有记录则以"初次认识"初始化。
pub fn observe_channel(db: &Database, channel: &str, external_id: &str) -> Result<()> {
    let now = Utc::now();
    let mut r = match load(db, channel, external_id)? {
        Some(r) => r,
        None => Relation {
            channel: channel.to_string(),
            external_id: external_id.to_string(),
            closeness: START_CLOSENESS,
            trust: START_TRUST,
            updated_at: now,
        },
    };
    r.closeness = (r.closeness + BUMP_CLOSENESS).min(1.0);
    r.trust = (r.trust + BUMP_TRUST).min(1.0);
    r.updated_at = now;
    save(db, &r)
}

pub fn load(db: &Database, channel: &str, external_id: &str) -> Result<Option<Relation>> {
    let mut stmt = db.conn().prepare(
        "SELECT closeness, trust, updated_at
         FROM relations WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |r| {
        Ok((
            r.get::<_, f32>(0)?,
            r.get::<_, f32>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    match rows.next() {
        Some(Ok((c, t, upd))) => Some(DateTime::parse_from_rfc3339(&upd).ok().map(|d| {
            d.with_timezone(&Utc)
        }))
        .flatten()
        .map(|upd| {
            Ok(Relation {
                channel: channel.to_string(),
                external_id: external_id.to_string(),
                closeness: c,
                trust: t,
                updated_at: upd,
            })
        })
        .transpose(),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn save(db: &Database, r: &Relation) -> Result<()> {
    db.conn().execute(
        "INSERT INTO relations (channel, external_id, closeness, trust, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(channel, external_id) DO UPDATE SET
           closeness  = excluded.closeness,
           trust      = excluded.trust,
           updated_at = excluded.updated_at",
        params![
            r.channel,
            r.external_id,
            r.closeness.clamp(0.0, 1.0),
            r.trust.clamp(0.0, 1.0),
            r.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// 世界心跳：全部关系按距上次更新的时间指数降温。
pub fn tick_decay(db: &Database) -> Result<()> {
    let now = Utc::now();
    let list = {
        let mut stmt = db.conn().prepare(
            "SELECT channel, external_id, closeness, trust, updated_at FROM relations",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f32>(2)?,
                r.get::<_, f32>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let x = rows.collect::<rusqlite::Result<Vec<_>>>();
        x?
    };

    for (ch, ext, c, t, upd) in list {
        let Some(prev) = DateTime::parse_from_rfc3339(&upd).ok().map(|d| d.with_timezone(&Utc)) else {
            continue;
        };
        let r = Relation {
            channel: ch.clone(),
            external_id: ext.clone(),
            closeness: c,
            trust: t,
            updated_at: prev,
        };
        let decayed = r.decayed(now);
        save(db, &decayed)?;
    }
    Ok(())
}

fn decay_factor(days: f64, half_life_days: f64) -> f32 {
    0.5f64.powf(days.max(0.0) / half_life_days) as f32
}

/// 亲密度 → 口语标签。
pub fn closeness_label(c: f32) -> &'static str {
    match c {
        _ if c >= 0.7 => "很亲近",
        _ if c >= 0.45 => "比较熟络",
        _ if c >= 0.25 => "有点熟",
        _ if c >= 0.1 => "普通朋友",
        _ => "还不熟",
    }
}

/// 信任度 → 口语标签。
pub fn trust_label(t: f32) -> &'static str {
    match t {
        _ if t >= 0.6 => "很信任",
        _ if t >= 0.35 => "较信任",
        _ if t >= 0.15 => "有所保留",
        _ => "还没怎么信任",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::db::session;

    #[test]
    fn observe_bumps_and_initializes() {
        let db = Database::in_memory().unwrap();
        let s = session::create(&db, "").unwrap();
        db.conn()
            .execute(
                "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                 VALUES ('telegram', '42', ?1, '2026-09-14T00:00:00Z', '2026-09-14T00:00:00Z')",
                params![s.id],
            )
            .unwrap();

        observe(&db, &s.id).unwrap();
        let r = load(&db, "telegram", "42").unwrap().unwrap();
        assert!((r.closeness - (START_CLOSENESS + BUMP_CLOSENESS)).abs() < 1e-4);
        assert!((r.trust - (START_TRUST + BUMP_TRUST)).abs() < 1e-4);

        observe(&db, &s.id).unwrap();
        let r2 = load(&db, "telegram", "42").unwrap().unwrap();
        assert!(r2.closeness > r.closeness, "每次来消息都升温");
        assert!(r2.closeness <= 1.0);
    }

    #[test]
    fn decay_cools_relation_over_time() {
        let db = Database::in_memory().unwrap();
        let s = session::create(&db, "").unwrap();
        db.conn()
            .execute(
                "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                 VALUES ('telegram', '42', ?1, '2026-09-14T00:00:00Z', '2026-09-14T00:00:00Z')",
                params![s.id],
            )
            .unwrap();
        observe(&db, &s.id).unwrap();

        // 把关系"冻结"在两周前，再跑一轮心跳 → 亲密度减半。
        db.conn()
            .execute(
                "UPDATE relations SET updated_at = ?1 WHERE channel = 'telegram' AND external_id = '42'",
                params![(Utc::now() - Duration::days(14)).to_rfc3339()],
            )
            .unwrap();
        let before = load(&db, "telegram", "42").unwrap().unwrap();
        tick_decay(&db).unwrap();
        let after = load(&db, "telegram", "42").unwrap().unwrap();
        assert!(after.closeness < before.closeness);
        assert!((after.closeness - before.closeness * 0.5).abs() < 1e-3, "半衰期 14 天");
    }

    #[test]
    fn labels_span_all_levels() {
        assert_eq!(closeness_label(0.8), "很亲近");
        assert_eq!(closeness_label(0.5), "比较熟络");
        assert_eq!(closeness_label(0.3), "有点熟");
        assert_eq!(closeness_label(0.1), "普通朋友");
        assert_eq!(closeness_label(0.0), "还不熟");
        assert_eq!(trust_label(0.8), "很信任");
        assert_eq!(trust_label(0.5), "较信任");
        assert_eq!(trust_label(0.2), "有所保留");
        assert_eq!(trust_label(0.0), "还没怎么信任");
    }
}