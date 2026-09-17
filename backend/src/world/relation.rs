//! 关系（按用户）：亲密度 × 信任度。交互会升温，久不往来会随时间衰减，
//! 与 [`crate::world::emotion`] 一样是"有遗忘、有冷却"的慢变量。
//!
//! 事件驱动模型：日常闲聊对关系影响很小（Neutral 仅微小增量），关键事件影响大——
//! 关心/暧昧大幅升温，矛盾/吵架乘法折损亲密度（深关系经得起吵）、信任折损更重
//! 且对低信任的关系伤得更狠，和解专门修复信任。事件类型由记忆提取的同一
//! 次 LLM 调用判定（见 [`crate::memory::extraction::MemoryExtraction::relation`]）。

use chrono::{DateTime, Duration, Utc};

use crate::db::relations as db_relations;
use crate::db::relations::RelationRow;
use crate::db::Database;
use crate::error::Result;

/// 首次认识时的初始亲密度 / 信任度。
pub const START_CLOSENESS: f32 = 0.15;
pub const START_TRUST: f32 = 0.05;
/// 日常闲聊的微小增量（封顶 1.0）——刻意做小，日常往来不再无差别大起大落。
pub const NEUTRAL_CLOSENESS: f32 = 0.005;
pub const NEUTRAL_TRUST: f32 = 0.002;
/// 关心/惦记/暖心话：亲密度温和上升，信任度良好上升。
pub const WARM_CLOSENESS: f32 = 0.08;
pub const WARM_TRUST: f32 = 0.06;
/// 暧昧/试探/私下亲密：亲密度明显上升。
pub const AMBIG_CLOSENESS: f32 = 0.15;
pub const AMBIG_TRUST: f32 = 0.06;
/// 矛盾/吵架——亲密度乘法折损（× 0.85，深关系经得起吵）；
/// 信任同样乘法折损，但对低信任的关系更狠（基础折损 + 信任抗造加成）。
pub const CONFLICT_CLOSENESS_FACTOR: f32 = 0.85;
pub const CONFLICT_TRUST_BASE: f32 = 0.62;
pub const CONFLICT_TRUST_FACTOR: f32 = 0.18;
/// 和解/道歉/示好：亲密度温和回升，信任显著修复。
pub const RECONCILE_CLOSENESS: f32 = 0.06;
pub const RECONCILE_TRUST: f32 = 0.12;
/// 负面事件伤害的关系地板，避免一次冲突把关系直接清零。
const MIN_CLOSENESS: f32 = 0.02;
const MIN_TRUST: f32 = 0.01;
/// 冷场的半衰期（天）。
const CLOSENESS_HALF_LIFE_DAYS: f64 = 14.0;
const TRUST_HALF_LIFE_DAYS: f64 = 40.0;

/// 本条用户消息对两人关系的影响事件（由记忆提取的同一次 LLM 调用判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationEvent {
    /// 日常闲聊 / 普通提问：几乎不改变关系。
    Neutral,
    /// 关心 / 惦记 / 暖心 / 感谢：信任与亲密度温和上升。
    Warm,
    /// 暧昧 / 试探 / 私下亲密：亲密度明显上升。
    Ambiguous,
    /// 矛盾 / 吵架 / 抱怨 / 失望 / 误会：亲密度乘法折损，信任折损更重。
    Conflict,
    /// 和解 / 道歉 / 示好：专门修复信任损伤。
    Reconcile,
}

impl RelationEvent {
    /// 把 LLM 输出的事件标签解析为枚举；未知标签一律回退到 Neutral。
    pub fn from_label(label: &str) -> RelationEvent {
        match label.trim().to_ascii_lowercase().as_str() {
            "warm" | "care" | "caring" | "nice" | "thank" | "thanks" => RelationEvent::Warm,
            "ambig" | "ambiguous" | "romantic" | "flirt" | "flirty" | "intimate" => {
                RelationEvent::Ambiguous
            }
            "conflict" | "fight" | "argument" | "angry" | "complain" | "upset" | "nip" => {
                RelationEvent::Conflict
            }
            "reconcile" | "reconciliation" | "apology" | "apologize" | "makeup" => {
                RelationEvent::Reconcile
            }
            _ => RelationEvent::Neutral,
        }
    }
}

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

impl From<RelationRow> for Relation {
    fn from(row: RelationRow) -> Self {
        Relation {
            channel: row.channel,
            external_id: row.external_id,
            closeness: row.closeness,
            trust: row.trust,
            updated_at: row.updated_at,
        }
    }
}

impl From<&Relation> for RelationRow {
    fn from(r: &Relation) -> Self {
        RelationRow {
            channel: r.channel.clone(),
            external_id: r.external_id.clone(),
            closeness: r.closeness,
            trust: r.trust,
            updated_at: r.updated_at,
        }
    }
}

/// 单次用户消息后的关系变化（无渠道映射的会话静默跳过）。
/// 默认按 Neutral（日常闲聊）处理——相当于"对方只是正常说话"的微小升温。
pub fn observe(db: &Database, session_id: &str) -> Result<()> {
    apply_event(db, session_id, RelationEvent::Neutral)
}

/// 按本条消息的关系事件类型调整关系（无渠道映射的会话静默跳过）。
pub fn apply_event(db: &Database, session_id: &str, event: RelationEvent) -> Result<()> {
    let Some((channel, external)) = crate::db::channel::channel_of(db, session_id)? else {
        return Ok(());
    };
    apply_event_channel(db, &channel, &external, event)
}

/// 对某位用户应用一次关系事件。没有记录则以"初次认识"初始化。
pub fn apply_event_channel(
    db: &Database,
    channel: &str,
    external_id: &str,
    event: RelationEvent,
) -> Result<()> {
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
    match event {
        // 日常闲聊：只留一个几乎不可感的脚印。
        RelationEvent::Neutral => {
            r.closeness = (r.closeness + NEUTRAL_CLOSENESS).min(1.0);
            r.trust = (r.trust + NEUTRAL_TRUST).min(1.0);
        }
        // 关心/暖心：信任与亲密一起回暖。
        RelationEvent::Warm => {
            r.closeness = (r.closeness + WARM_CLOSENESS).min(1.0);
            r.trust = (r.trust + WARM_TRUST).min(1.0);
        }
        // 暧昧/亲密：亲密度大幅升温。
        RelationEvent::Ambiguous => {
            r.closeness = (r.closeness + AMBIG_CLOSENESS).min(1.0);
            r.trust = (r.trust + AMBIG_TRUST).min(1.0);
        }
        // 矛盾/吵架：亲密度乘法折损；信任折损更重，且低信任关系伤得更深。
        RelationEvent::Conflict => {
            let trust_factor = CONFLICT_TRUST_BASE + CONFLICT_TRUST_FACTOR * r.trust;
            r.closeness = (r.closeness * CONFLICT_CLOSENESS_FACTOR).max(MIN_CLOSENESS);
            r.trust = (r.trust * trust_factor).max(MIN_TRUST);
        }
        // 和解/道歉：信任显著修复。
        RelationEvent::Reconcile => {
            r.closeness = (r.closeness + RECONCILE_CLOSENESS).min(1.0);
            r.trust = (r.trust + RECONCILE_TRUST).min(1.0);
        }
    }
    r.updated_at = now;
    save(db, &r)
}

pub fn load(db: &Database, channel: &str, external_id: &str) -> Result<Option<Relation>> {
    Ok(db_relations::load(db, channel, external_id)?.map(Relation::from))
}

pub fn save(db: &Database, r: &Relation) -> Result<()> {
    db_relations::save(db, &r.into())
}

/// 世界心跳：全部关系按距上次更新的时间指数降温。
pub fn tick_decay(db: &Database) -> Result<()> {
    let now = Utc::now();
    for row in db_relations::list_all(db)? {
        let decayed = Relation::from(row).decayed(now);
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

    /// 建一个带渠道映射的会话（relation 按 channel+external 存取）。
    fn session_with_channel(db: &Database, external_id: &str) -> String {
        crate::db::channel::get_or_create_session(db, "telegram", external_id)
            .unwrap()
            .id
    }

    #[test]
    fn observe_moves_very_little() {
        let db = Database::in_memory().unwrap();
        let s = session_with_channel(&db, "42");

        observe(&db, &s).unwrap();
        let r = load(&db, "telegram", "42").unwrap().unwrap();
        assert!((r.closeness - (START_CLOSENESS + NEUTRAL_CLOSENESS)).abs() < 1e-6);
        assert!((r.trust - (START_TRUST + NEUTRAL_TRUST)).abs() < 1e-6);

        // 连发 10 条日常消息，总升温仍赶不上一条关心话（日常影响被刻意做小）。
        for _ in 0..10 {
            observe(&db, &s).unwrap();
        }
        let r2 = load(&db, "telegram", "42").unwrap().unwrap();
        let daily_gain = r2.closeness - r.closeness;
        assert!(daily_gain > 0.0, "日常往来仍有累积");
        assert!(daily_gain < WARM_CLOSENESS, "十条闲聊 < 一条关心的升温");
        assert!(r2.closeness <= 1.0);
    }

    #[test]
    fn key_events_move_relation_much_more() {
        let db = Database::in_memory().unwrap();
        let s = session_with_channel(&db, "42");
        apply_event(&db, &s, RelationEvent::Neutral).unwrap();
        let neutral = load(&db, "telegram", "42").unwrap().unwrap();

        // 一条暧昧消息远大于一条日常闲聊。
        let s = session_with_channel(&db, "43");
        apply_event(&db, &s, RelationEvent::Ambiguous).unwrap();
        let ambig = load(&db, "telegram", "43").unwrap().unwrap();
        let neutral_gain = neutral.closeness - START_CLOSENESS;
        assert!(ambig.closeness - START_CLOSENESS > neutral_gain * 20.0);
    }

    #[test]
    fn conflict_multiplies_and_hurts_low_trust_harder() {
        let db = Database::in_memory().unwrap();
        let s = session_with_channel(&db, "42");

        // 先把关系抬到一个较熟的档位。
        apply_event(&db, &s, RelationEvent::Warm).unwrap();
        apply_event(&db, &s, RelationEvent::Warm).unwrap();
        let before = load(&db, "telegram", "42").unwrap().unwrap();

        apply_event(&db, &s, RelationEvent::Conflict).unwrap();
        let after = load(&db, "telegram", "42").unwrap().unwrap();
        assert!(after.closeness < before.closeness);
        assert!(after.trust < before.trust);
        assert!((after.closeness - before.closeness * CONFLICT_CLOSENESS_FACTOR).abs() < 1e-4);

        // 低信任关系同一趟冲突伤得更重：信任折损因子随信任度上升而变温和。
        let expected_trust =
            before.trust * (CONFLICT_TRUST_BASE + CONFLICT_TRUST_FACTOR * before.trust);
        assert!((after.trust - expected_trust).abs() < 1e-4);
    }

    #[test]
    fn reconcile_restores_trust_after_conflict() {
        let db = Database::in_memory().unwrap();
        let s = session_with_channel(&db, "42");
        apply_event(&db, &s, RelationEvent::Warm).unwrap();
        apply_event(&db, &s, RelationEvent::Warm).unwrap();
        let armed = load(&db, "telegram", "42").unwrap().unwrap();

        apply_event(&db, &s, RelationEvent::Conflict).unwrap();
        let conflicted = load(&db, "telegram", "42").unwrap().unwrap();

        apply_event(&db, &s, RelationEvent::Reconcile).unwrap();
        let healed = load(&db, "telegram", "42").unwrap().unwrap();
        assert!(healed.trust > conflicted.trust);
        assert!(healed.closeness > conflicted.closeness);
        assert!(healed.trust > armed.trust, "和解/道歉能显著修复信任");
    }

    #[test]
    fn event_labels_parse() {
        assert_eq!(RelationEvent::from_label("warm"), RelationEvent::Warm);
        assert_eq!(RelationEvent::from_label("  AMBIG "), RelationEvent::Ambiguous);
        assert_eq!(RelationEvent::from_label("conflict"), RelationEvent::Conflict);
        assert_eq!(RelationEvent::from_label("apology"), RelationEvent::Reconcile);
        assert_eq!(RelationEvent::from_label("whatever"), RelationEvent::Neutral);
        assert_eq!(RelationEvent::from_label(""), RelationEvent::Neutral);
    }

    #[test]
    fn decay_cools_relation_over_time() {
        let db = Database::in_memory().unwrap();
        let s = session_with_channel(&db, "42");
        observe(&db, &s).unwrap();

        // 把关系"冻结"在两周前，再跑一轮心跳 → 亲密度减半。
        let frozen = load(&db, "telegram", "42").unwrap().unwrap();
        save(
            &db,
            &Relation {
                updated_at: Utc::now() - Duration::days(14),
                ..frozen
            },
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