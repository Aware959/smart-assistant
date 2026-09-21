//! 关系（按用户）：亲密度 × 信任度。交互会升温，久不往来会随时间衰减，
//! 是"有遗忘、有冷却"的慢变量。
//!
//! 事件驱动模型：日常闲聊对关系影响很小（Neutral 仅微小增量），关键事件影响大——
//! 关心/暧昧大幅升温，矛盾/吵架乘法折损亲密度（深关系经得起吵）、信任折损更重
//! 且对低信任的关系伤得更狠，和解专门修复信任。事件类型由记忆提取的同一
//! 次 LLM 调用判定（见 [`crate::core::memory::user_memory_extraction::MemoryExtraction::relation`]）。
//!
//! 本模块是**纯逻辑**：不碰数据库。**aiworld 已整体拆出等待重新设计**：值的水合、
//! 演化接线（`apply_event` 触发）与 `db::relations` 写穿均已移除，新设计定稿后
//! 在 `runtime` 重新装配。

use chrono::{DateTime, Duration, Utc};

use crate::db::relations::RelationRow;

/// 本条用户消息对两人关系的影响事件（由记忆判定给出标签）。
///
/// 放在关系域：它同时被**记忆层**（判定产出，见
/// [`crate::core::memory::user_memory_extraction::MemoryExtraction::relation`]）与
/// 关系演化（[`apply_event`]）使用，是两者之间的契约类型。
/// 具体怎么调整关系（升温/折损系数）是本模块的事。
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

/// 一段"按用户"的关系状态：AI 与某个外部用户（channel × external_id）的
/// 亲密度 / 信任度两个慢变量及其最近更新时间。**不持有值**——本模块只负责
/// 演化规则，结构直接对应 `db::relations` 行（见 `From` 转换）。
///
/// 两轴语义（都在 0~1，`apply_event` 上抬封顶 1.0，`decayed` 只降不升）：
/// - `closeness`（亲密度）：走得有多近。日常/关心/暧昧上抬，冲突乘性折损；
/// - `trust`（信任度）：有多可信。温和上抬但折损更狠，只有和解才能显著修复。
#[derive(Debug, Clone)]
pub struct Relation {
    /// 渠道标识（telegram / ilink ……），与 `external_id` 一起唯一定位一个用户。
    pub channel: String,
    /// 渠道里的用户 id（TG chat_id / iLink 用户号）——`(channel, external_id)` 是关系主键。
    pub external_id: String,
    /// 亲密度 0~1：日常闲聊 / 关心 / 暧昧事件让它上升，冲突把它乘性折损。
    pub closeness: f32,
    /// 信任度 0~1：日常与关心温和上升，冲突折损比亲密度更重，且只靠和解/道歉修复。
    pub trust: f32,
    /// 最近一次状态变化时刻（初始认识 / 事件落点 / 衰减更新都写这里）。
    /// 过期衰减 [`Relation::decayed`] 按它距现在的天数套半衰期算降温。
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

    /// 按事件类型演化关系：值进值出，`now` 作为更新时刻落回 `updated_at`。
    pub fn apply_event(mut self, event: RelationEvent, now: DateTime<Utc>) -> Relation {
        match event {
            // 日常闲聊：只留一个几乎不可感的脚印。
            RelationEvent::Neutral => {
                self.closeness = (self.closeness + NEUTRAL_CLOSENESS).min(1.0);
                self.trust = (self.trust + NEUTRAL_TRUST).min(1.0);
            }
            // 关心/暖心：信任与亲密一起回暖。
            RelationEvent::Warm => {
                self.closeness = (self.closeness + WARM_CLOSENESS).min(1.0);
                self.trust = (self.trust + WARM_TRUST).min(1.0);
            }
            // 暧昧/亲密：亲密度大幅升温。
            RelationEvent::Ambiguous => {
                self.closeness = (self.closeness + AMBIG_CLOSENESS).min(1.0);
                self.trust = (self.trust + AMBIG_TRUST).min(1.0);
            }
            // 矛盾/吵架：亲密度乘法折损；信任折损更重，且低信任关系伤得更深。
            RelationEvent::Conflict => {
                let trust_factor = CONFLICT_TRUST_BASE + CONFLICT_TRUST_FACTOR * self.trust;
                self.closeness = (self.closeness * CONFLICT_CLOSENESS_FACTOR).max(MIN_CLOSENESS);
                self.trust = (self.trust * trust_factor).max(MIN_TRUST);
            }
            // 和解/道歉：信任显著修复。
            RelationEvent::Reconcile => {
                self.closeness = (self.closeness + RECONCILE_CLOSENESS).min(1.0);
                self.trust = (self.trust + RECONCILE_TRUST).min(1.0);
            }
        }
        self.updated_at = now;
        self
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

    fn seeded(channel: &str, external: &str) -> Relation {
        Relation {
            channel: channel.to_string(),
            external_id: external.to_string(),
            closeness: START_CLOSENESS,
            trust: START_TRUST,
            updated_at: Utc::now(),
        }
    }

    /// 一条 new 消息后的关系（默认 Neutral）。
    fn after_first(r: Relation, event: RelationEvent) -> Relation {
        r.apply_event(event, Utc::now())
    }

    #[test]
    fn neutral_moves_very_little() {
        let r = after_first(seeded("telegram", "42"), RelationEvent::Neutral);
        assert!((r.closeness - (START_CLOSENESS + NEUTRAL_CLOSENESS)).abs() < 1e-6);
        assert!((r.trust - (START_TRUST + NEUTRAL_TRUST)).abs() < 1e-6);

        // 连发 10 条日常消息，总升温仍赶不上一条关心话（日常影响被刻意做小）。
        let mut daily = seeded("telegram", "42");
        for _ in 0..10 {
            daily = daily.apply_event(RelationEvent::Neutral, Utc::now());
        }
        let daily_gain = daily.closeness - r.closeness;
        assert!(daily_gain > 0.0, "日常往来仍有累积");
        assert!(daily_gain < WARM_CLOSENESS, "十条闲聊 < 一条关心的升温");
        assert!(daily.closeness <= 1.0);
    }

    #[test]
    fn key_events_move_relation_much_more() {
        let neutral = after_first(seeded("telegram", "42"), RelationEvent::Neutral);
        let ambig = after_first(seeded("telegram", "43"), RelationEvent::Ambiguous);
        let neutral_gain = neutral.closeness - START_CLOSENESS;
        assert!(ambig.closeness - START_CLOSENESS > neutral_gain * 20.0);
    }

    #[test]
    fn conflict_multiplies_and_hurts_low_trust_harder() {
        let before = seeded("telegram", "42")
            .apply_event(RelationEvent::Warm, Utc::now())
            .apply_event(RelationEvent::Warm, Utc::now());
        let after = before.clone().apply_event(RelationEvent::Conflict, Utc::now());
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
        let armed = seeded("telegram", "42")
            .apply_event(RelationEvent::Warm, Utc::now())
            .apply_event(RelationEvent::Warm, Utc::now());
        let conflicted = armed.clone().apply_event(RelationEvent::Conflict, Utc::now());
        let healed = conflicted.clone().apply_event(RelationEvent::Reconcile, Utc::now());
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
        assert_eq!(RelationEvent::from_label("care"), RelationEvent::Warm);
        assert_eq!(RelationEvent::from_label("whatever"), RelationEvent::Neutral);
        assert_eq!(RelationEvent::from_label(""), RelationEvent::Neutral);
    }

    #[test]
    fn decay_cools_relation_over_time() {
        let frozen = Relation {
            updated_at: Utc::now() - Duration::days(14),
            ..seeded("telegram", "42")
        };
        let before = frozen.closeness;
        let after = frozen.decayed(Utc::now());
        assert!(after.closeness < before);
        assert!((after.closeness - before * 0.5).abs() < 1e-3, "半衰期 14 天");
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