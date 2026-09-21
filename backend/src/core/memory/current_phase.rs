//! 时间与作息：让 AI 拥有"进行时"感知的画像推断。
//!
//! `messages.created_at` 携带的是通道报文里的真实用户时刻（见 [`crate::channels`]），
//! 这份时间线是画像推断的唯一依据：`infer_from_hours` 由一组 UTC 小时算出
//! "对方时区偏移 + 最活跃小时"。
//!
//! 本模块是**纯逻辑**：只做推断与时段映射，不碰数据库。**aiworld 已整体拆出等待
//! 重新设计**：当前的喂数（消息时刻）、画像水合与写穿接线均已移除，新设计定稿后
//! 在 `runtime` 重新装配。

use std::collections::HashMap;

// ---------- 时段 ----------

/// 一个时段的定义：数值区间 + 中文名。直接内嵌在 [`Phase`] 变体上，
/// 时段（几点到几点）与名称（叫什么）收敛到**单一数据源**——此前用两张分立表
/// （数值→时段、时段→名称）各自维护，存在区间与标签漂移的风险。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseDef {
    /// 覆盖的钟点闭区间（0-23）：`hours.0..=hours.1`。
    pub hours: (u32, u32),
    /// 中文时段名（渲染进提示词的纯文本）。
    pub label: &'static str,
}

/// 一天里的阶段（按某时区的钟表小时划分）。
///
/// 每个变体直接携带自己的 [`PhaseDef`]——取值与取标签都从同一份数据，不需要
/// 在外部再维护一张"时间段→标签"的转换表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    DeepNight(PhaseDef),
    EarlyMorning(PhaseDef),
    Morning(PhaseDef),
    Forenoon(PhaseDef),
    Noon(PhaseDef),
    Afternoon(PhaseDef),
    Evening(PhaseDef),
    Night(PhaseDef),
}

impl Phase {
    /// 全部时段（顺序即一天顺序；供遍历/校验与 `phase_of` 数据驱动匹配）。
    pub const ALL: [Phase; 8] = [
        Phase::DeepNight(PhaseDef { hours: (0, 4), label: "深夜" }),
        Phase::EarlyMorning(PhaseDef { hours: (5, 6), label: "清晨" }),
        Phase::Morning(PhaseDef { hours: (7, 9), label: "早上" }),
        Phase::Forenoon(PhaseDef { hours: (10, 11), label: "上午" }),
        Phase::Noon(PhaseDef { hours: (12, 13), label: "中午" }),
        Phase::Afternoon(PhaseDef { hours: (14, 17), label: "下午" }),
        Phase::Evening(PhaseDef { hours: (18, 19), label: "傍晚" }),
        Phase::Night(PhaseDef { hours: (20, 23), label: "晚上" }),
    ];

    /// 取内嵌的时段定义（数值区间 + 名称）。
    pub fn def(&self) -> &PhaseDef {
        match self {
            Phase::DeepNight(d)
            | Phase::EarlyMorning(d)
            | Phase::Morning(d)
            | Phase::Forenoon(d)
            | Phase::Noon(d)
            | Phase::Afternoon(d)
            | Phase::Evening(d)
            | Phase::Night(d) => d,
        }
    }

    /// 中文时段名（就是内嵌在定义里的那个，不再查第二张表）。
    pub fn label(&self) -> &'static str {
        self.def().label
    }
}

/// 0-23 小时 → 时段。
pub fn phase_of(hour: u32) -> Phase {
    Phase::ALL
        .into_iter()
        .find(|p| {
            let def = p.def();
            def.hours.0 <= hour && hour <= def.hours.1
        })
        .unwrap_or(Phase::ALL[7])
}

// ---------- 画像推断 ----------

/// 从一组历史 user 消息的 UTC 小时推断作息画像。
///
/// 返回 `(offset_minutes, active_hour, 观测数)`；小时列表为空（还没有足够数据）时
/// 返回 None，调用方应静默跳过写穿。
///
/// 时区推断假设"对方当地活跃窗口通常是 9:00~23:59"，对每个候选偏移统计该窗口内
/// 的消息占比，取占比最高的偏移；同分偏好东八区（更稳的默认可协商）。
pub fn infer_from_hours(hours: &[u32]) -> Option<(i32, u32, usize)> {
    if hours.is_empty() {
        return None;
    }

    let mut counts = HashMap::<u32, usize>::new();
    for &h in hours {
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
    Some((offset_minutes, active_hour, hours.len()))
}

/// 同分时偏好东八区，其次偏移绝对值小。
fn prefer(a: &i32, b: &i32) -> bool {
    let da = (8 - a).unsigned_abs();
    let db = (8 - b).unsigned_abs();
    da < db || (da == db && a.unsigned_abs() < b.unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_boundaries() {
        assert_eq!(phase_of(0).label(), "深夜");
        assert_eq!(phase_of(4).label(), "深夜");
        assert_eq!(phase_of(5).label(), "清晨");
        assert_eq!(phase_of(7).label(), "早上");
        assert_eq!(phase_of(11).label(), "上午");
        assert_eq!(phase_of(13).label(), "中午");
        assert_eq!(phase_of(17).label(), "下午");
        assert_eq!(phase_of(19).label(), "傍晚");
        assert_eq!(phase_of(23).label(), "晚上");
        assert!(!phase_of(0).label().is_empty());
    }

    #[test]
    fn phases_cover_the_day_exactly_once() {
        // 单一数据源不变量：8 个时段正好把 0-23 覆盖完，且没有任何小时命中两个时段。
        for hour in 0..24 {
            let hits = Phase::ALL
                .iter()
                .filter(|p| {
                    let def = p.def();
                    def.hours.0 <= hour && hour <= def.hours.1
                })
                .collect::<Vec<_>>();
            assert_eq!(hits.len(), 1, "小时 {hour} 应恰好命中一个时段");
            assert_eq!(hits[0].label(), phase_of(hour).label());
        }
    }

    #[test]
    fn infers_east8_offset_and_active_hour() {
        // 我们的北京用户：UTC 3 点/4 点发消息 = 东八区 11/12 点（上午活跃）。
        let p = infer_from_hours(&[3, 4, 3]).unwrap();
        assert_eq!(p.0, 8 * 60, "应推断出东八区");
        assert_eq!(p.1, 3, "UTC 3 点为最活跃小时");
        assert_eq!(p.2, 3);
    }

    #[test]
    fn empty_hours_yield_none() {
        assert!(infer_from_hours(&[]).is_none());
    }
}