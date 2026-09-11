//! 节奏控制：随机间隔 + 安静时段。
//!
//! 每轮从 `[PROACTIVE_MIN_MINUTES, PROACTIVE_MAX_MINUTES]` 区间均匀随机抽取一个
//! 时长作为下一轮等待（不规律，像真人）。若恰逢安静时段，则直接睡到时段结束，
//! 期间不做任何评估。

use std::time::Duration;

use chrono::{DateTime, Local, Timelike};

use crate::config::Config;

/// 返回下一轮应当等待的时长。
///
/// 安静时段内 → 睡到安静窗口结束（多留 30s 缓冲）；否则 → 随机间隔。
pub fn next_sleep() -> Duration {
    let now = Local::now();
    if let Some(rem) = quiet_remainder(now.hour(), now.minute()) {
        rem + Duration::from_secs(30)
    } else {
        random_interval()
    }
}

/// 当前本机时刻是否处于安静时段。
pub fn in_quiet_hours(now: DateTime<Local>) -> bool {
    quiet_remainder(now.hour(), now.minute()).is_some()
}

/// 距安静时段结束的等待时长（分钟）；不在安静时段内返回 None。
///
/// quiet 窗口为半开区间 `[start, end)`：start < end 为当日窗口，
/// start > end 表示跨午夜（如 23-7 → 23:00~次日 6:59），start == end 视为禁用。
pub fn quiet_remainder(hour: u32, minute: u32) -> Option<Duration> {
    quiet_remainder_for(Config::get().proactive_quiet_hours, hour, minute)
}

/// 纯函数版，便于测试。
pub fn quiet_remainder_for(q: (u32, u32), hour: u32, minute: u32) -> Option<Duration> {
    let (start, end) = q;
    if start == end {
        return None;
    }
    let mins = hour as i64 * 60 + minute as i64;

    let in_window = if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    };
    if !in_window {
        return None;
    }
    let until_min = if start > end && hour >= start {
        1440 + end as i64 * 60 - mins
    } else {
        end as i64 * 60 - mins
    };
    Some(Duration::from_secs(until_min.max(0) as u64 * 60))
}

/// `[min, max]`（分钟）区间上的均匀随机；min>max 时对调，相等时取该值。
fn random_interval() -> Duration {
    let cfg = Config::get();
    let (mut lo, mut hi) = (cfg.proactive_min_minutes, cfg.proactive_max_minutes);
    if lo > hi {
        std::mem::swap(&mut lo, &mut hi);
    }
    let span = (hi - lo).max(1) as u64;
    let minutes = (lo as u64).saturating_add(prand() % span);
    Duration::from_secs(minutes.max(1) * 60)
}

/// 简易线性同余伪随机数，专用于节奏抖动（非安全用途），避免新增依赖。
pub(crate) fn prand() -> u64 {
    use std::cell::Cell;

    thread_local! {
        static SEED: Cell<u64> = Cell::new(0x9E37_79B9_7F4A_7C15 ^ seed_from_time());
    }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x
    })
}

fn seed_from_time() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_window_without_wrap() {
        // (0,23)：10:00 在窗口内 → 剩余到 23:00 = 13h。
        assert_eq!(
            quiet_remainder_for((0, 23), 10, 0),
            Some(Duration::from_secs(13 * 60 * 60))
        );
        // (7,9)：8:30 安静（剩 30min），10:00 不安静。
        assert_eq!(
            quiet_remainder_for((7, 9), 8, 30),
            Some(Duration::from_secs(30 * 60))
        );
        assert!(quiet_remainder_for((7, 9), 10, 0).is_none());
    }

    #[test]
    fn quiet_window_crosses_midnight() {
        // 23-7：23:00 起安静到次日 07:00。
        assert_eq!(
            quiet_remainder_for((23, 7), 23, 0),
            Some(Duration::from_secs(8 * 60 * 60))
        );
        // 次日凌晨同样安静（剩 5h）。
        assert_eq!(
            quiet_remainder_for((23, 7), 2, 0),
            Some(Duration::from_secs(5 * 60 * 60))
        );
        // 10:00 不安静。
        assert!(quiet_remainder_for((23, 7), 10, 0).is_none());
    }

    #[test]
    fn quiet_disabled_when_equal() {
        assert!(quiet_remainder_for((7, 7), 7, 0).is_none());
    }

    #[test]
    fn interval_stays_within_bounds() {
        // 无法轻易改全局 config；这里只验证随机函数在默认边界内不 panic。
        let _ = random_interval();
    }
}