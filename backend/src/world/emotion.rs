//! 情绪模型：效价（好 ↔ 坏）乘精力（充沛 ↔ 疲惫）两个维度。
//!
//! 情绪是**有张力的、会自己回落的**状态：任何情绪都会随时间指数式地向
//! 基线（valence 0、energy 0.5）回归——一个人不会永远亢奋，也不会永远低落。
//! 交互与事件通过 [`Mood::shift`] 短时把它推离基线，随后由世界心跳的
//! [`decay`] 慢慢拉回来。

/// -1..1：大于 0 偏积极，小于 0 偏消极。
pub static NEUTRAL: Mood = Mood {
    valence: 0.0,
    energy: 0.5,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mood {
    pub valence: f32,
    pub energy: f32,
}

impl Mood {
    pub fn neutral() -> Self {
        NEUTRAL
    }

    /// 随时间向基线回归。`hours` 为自上次记录以来经过的小时数。
    /// 效价半衰期约 8 小时、精力约 4 小时。
    pub fn decayed(&self, hours: f64) -> Self {
        let v_span = half_life(8.0, hours.max(0.0));
        let e_span = half_life(4.0, hours.max(0.0));
        Mood {
            valence: self.valence * v_span,
            energy: 0.5 + (self.energy - 0.5) * e_span,
        }
    }

    /// 事件引起的即时偏移（dv：心情，de：精力），并夹取到合法区间。
    pub fn shifted(&self, dv: f32, de: f32) -> Self {
        Mood {
            valence: (self.valence + dv).clamp(-1.0, 1.0),
            energy: (self.energy + de).clamp(0.0, 1.0),
        }
    }
}

/// 指数衰减系数：`0.5^(hours / half_life_hours)` ∈ (0, 1]。
fn half_life(half_life_hours: f64, hours: f64) -> f32 {
    0.5f64.powf(hours / half_life_hours) as f32
}

/// 给提示词的一句话心情描述（口语化）。
pub fn label(m: &Mood) -> String {
    let feel = match m.valence {
        v if v > 0.35 => "心情不错",
        v if v > 0.1 => "有点开心",
        v if v >= -0.35 => "还算平静",
        _ => "心情有点低落",
    };
    let energy = match m.energy {
        e if e > 0.6 => "精神饱满",
        e if e >= 0.4 => "精力一般",
        _ => "有点疲惫",
    };
    format!("{feel}，{energy}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn neutral_is_at_baseline() {
        assert!(close(NEUTRAL.valence, 0.0));
        assert!(close(NEUTRAL.energy, 0.5));
    }

    #[test]
    fn decay_pulls_back_to_baseline() {
        let happy = Mood {
            valence: 0.8,
            energy: 0.9,
        };
        let after = happy.decayed(16.0); // 效价 2 个半衰期 → 1/4；精力 4 个半衰期 → 1/16
        assert!(close(after.valence, 0.8 * 0.25));
        assert!(close(after.energy, 0.5 + 0.4 * 0.5f64.powf(16.0 / 4.0) as f32));
    }

    #[test]
    fn decay_never_passes_baseline_sign() {
        let low = Mood {
            valence: -0.6,
            energy: 0.2,
        };
        let after = low.decayed(100.0);
        assert!(after.valence < 0.0);
        assert!(after.energy > 0.2 && after.energy <= 0.5);
    }

    #[test]
    fn decay_of_long_period_is_flat_baseline() {
        let m = Mood {
            valence: 1.0,
            energy: 0.0,
        };
        let after = m.decayed(24.0 * 30.0);
        assert!(close(after.valence, 0.0));
        assert!(close(after.energy, 0.5));
    }

    #[test]
    fn shift_clamps_to_range() {
        let m = NEUTRAL.shifted(5.0, -2.0);
        assert!(close(m.valence, 1.0));
        assert!(close(m.energy, 0.0));
    }

    #[test]
    fn label_is_never_empty_and_snaps_to_tone() {
        assert!(!label(&NEUTRAL).is_empty());
        assert!(label(&Mood { valence: 0.9, energy: 0.9 }).contains("不错"));
        assert!(label(&Mood { valence: -0.9, energy: 0.1 }).contains("低落"));
    }
}