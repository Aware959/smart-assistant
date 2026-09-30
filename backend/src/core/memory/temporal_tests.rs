use chrono::{DateTime, Duration, Utc};

use super::*;

/// 与实现同款的"今日零点"基座，避免跨午夜竞态导致断言抖动。
fn base_today() -> DateTime<Utc> {
    Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
}

/// 断言命中：层级正确、到期时刻落在 `[lo, hi]` 闭区间（跨午夜容差 ±1 天）。
fn assert_anchor(content: &str, expect_tier: &str, lo: DateTime<Utc>, hi: DateTime<Utc>) {
    let (tier, deadline) = temporal_anchor(content).unwrap_or_else(|| panic!("应命中：{content}"));
    assert_eq!(tier, expect_tier, "层级不符：{content}");
    assert!(
        deadline >= lo - Duration::days(1) && deadline <= hi + Duration::days(1),
        "到期时刻 {deadline} 不在预期区间 [{lo}, {hi}]：{content}"
    );
}

#[test]
fn immediate_words_expire_in_hours() {
    let now = Utc::now();
    assert_anchor("对方待会儿去开会", "short", now, now + Duration::hours(2));
    assert_anchor("稍后联系", "short", now, now + Duration::hours(2));
    assert_anchor("马上去拿快递", "short", now, now + Duration::hours(2));
}

#[test]
fn today_anchors_expire_at_end_of_day() {
    let base = base_today();
    assert_anchor(
        "对方今天要上班",
        "short",
        base + Duration::days(1),
        base + Duration::days(1),
    );
    assert_anchor(
        "今晚加班",
        "short",
        base + Duration::days(1),
        base + Duration::days(1),
    );
    assert_anchor(
        "对方今天去医院",
        "short",
        base + Duration::days(1),
        base + Duration::days(1),
    );
}

#[test]
fn tomorrow_anchors_expire_after_tomorrow() {
    let base = base_today();
    assert_anchor(
        "对方明天要见面",
        "intent",
        base + Duration::days(2),
        base + Duration::days(2),
    );
    assert_anchor(
        "明晚有个聚会",
        "intent",
        base + Duration::days(2),
        base + Duration::days(2),
    );
}

#[test]
fn day_after_expire_after_day_after() {
    let base = base_today();
    assert_anchor(
        "后天答辩",
        "intent",
        base + Duration::days(3),
        base + Duration::days(3),
    );
}

#[test]
fn weekend_this_week_next_week_next_month() {
    let base = base_today();
    assert_anchor(
        "对方周末要回老家",
        "intent",
        base + Duration::days(4),
        base + Duration::days(4),
    );
    assert_anchor(
        "这周要交周报",
        "intent",
        base + Duration::days(8),
        base + Duration::days(8),
    );
    assert_anchor(
        "对方下周要考试",
        "intent",
        base + Duration::days(10),
        base + Duration::days(10),
    );
    assert_anchor(
        "下个月出差",
        "intent",
        base + Duration::days(40),
        base + Duration::days(40),
    );
}

#[test]
fn numeric_horizons_are_resolved() {
    let now = Utc::now();
    let base = base_today();
    assert_anchor("对方 2 小时后下班", "short", now, now + Duration::hours(2));
    assert_anchor(
        "三天后见面",
        "intent",
        base + Duration::days(4),
        base + Duration::days(4),
    );
    assert_anchor(
        "对方两周后考试",
        "intent",
        base + Duration::days(15),
        base + Duration::days(15),
    );
    assert_anchor(
        "一个月后体检",
        "intent",
        base + Duration::days(31),
        base + Duration::days(31),
    );
}

#[test]
fn past_anchors_are_not_ephemeral() {
    // "前"是已成事实（现状已定型），不得当临时态处理。
    assert_eq!(temporal_anchor("对方三个月前辞职了"), None);
    assert_eq!(temporal_anchor("昨天到的快递"), None);
    assert_eq!(temporal_anchor("刚才吃完饭"), None);
}

#[test]
fn recurring_phrases_are_long_term() {
    // 反复性表达是长期事实模板，不是单点时效。
    assert_eq!(temporal_anchor("对方每天通勤一小时"), None);
    assert_eq!(temporal_anchor("每周四开会"), None);
    assert_eq!(temporal_anchor("经常加班到十点"), None);
}

#[test]
fn undefined_future_is_not_a_hard_anchor() {
    assert_eq!(temporal_anchor("改天再约"), None);
    assert_eq!(temporal_anchor("有空一起去"), None);
    assert_eq!(temporal_anchor("以后再说"), None);
}

#[test]
fn plain_facts_unaffected() {
    assert_eq!(temporal_anchor("对方最喜欢的颜色是蓝色"), None);
    assert_eq!(temporal_anchor("对方养了一只叫豆豆的猫"), None);
    assert_eq!(temporal_anchor(""), None);
}
