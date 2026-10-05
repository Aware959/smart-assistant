//! `core/memory/mod.rs` 的纯逻辑测试：记忆寿命的机械判断（种类默认窗口 + 时间锚覆盖）。
//!
//! 写入路径上的 embedding / DB 往返需要网络与磁盘，不在这里造；能离线确定的
//! 只有"种类 + 内容 → 到期时间"和"距离 → 相似度"这两处。

use super::*;

fn expires_of(kind: MemoryKind, content: &str) -> Option<String> {
    resolve_lifetime(kind, content)
}

/// trait / preference 定义"对方是谁"，没有到期日——过期只会让 AI 在对方没变的情况下
/// 失忆。
#[test]
fn trait_and_preference_never_expire() {
    assert_eq!(expires_of(MemoryKind::Trait, "对方住在上海"), None);
    assert_eq!(expires_of(MemoryKind::Preference, "希望被称呼为阿离"), None);
}

/// routine / plan 各自有界：作息会变，意向不落实就是过期信息。
#[test]
fn bounded_kinds_get_default_windows() {
    let routine = expires_of(MemoryKind::Routine, "午休时间是中午12点到下午2点")
        .expect("routine 应有默认期限");
    // 刻意不带任何时间词：内容里出现"下个月"这类词会走时间锚分支，
    // 测的就不是种类的默认窗口了。
    let plan = expires_of(MemoryKind::Plan, "考虑换个城市生活").expect("plan 应有默认期限");

    // 距今天数按自然日算，允许跨时区的半天误差。
    let whole_days = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("到期时间应为 RFC3339")
            .signed_duration_since(chrono::Utc::now())
            .num_hours()
            / 24
    };
    let routine_days = whole_days(&routine);
    let plan_days = whole_days(&plan);
    let expect = |kind: MemoryKind, actual: i64| {
        let want = kind.default_lifetime_days().unwrap();
        assert!(
            (actual - want).abs() <= 1,
            "{kind:?} 默认 {want} 天，实际 {actual} 天（{s}）",
            s = if kind == MemoryKind::Routine {
                &routine
            } else {
                &plan
            }
        );
    };
    expect(MemoryKind::Routine, routine_days);
    expect(MemoryKind::Plan, plan_days);
    assert!(
        routine_days > plan_days,
        "routine 的窗口应长于 plan：{routine} vs {plan}"
    );
}

/// 强时间锚压过种类的默认窗口：模型写"下周要体检"时种类可能是 trait，但到期日必须
/// 按锚点算，否则它会带着永不过期活很久。
#[test]
fn temporal_anchor_overrides_default_window() {
    for kind in MemoryKind::ALL {
        let anchored = expires_of(kind, "对方明天要去体检")
            .unwrap_or_else(|| panic!("{kind:?} 带时间锚也应有到期时间"));
        assert!(
            anchored.as_str() > chrono::Utc::now().to_rfc3339().as_str(),
            "{kind:?} 的到期时间应在将来：{anchored}"
        );
    }
}

/// 时间锚判定不能被"trait 永不过期"反向污染：同一种类下，两条只差一个时间词的内容
/// 到期行为就必须不同——这正是过去按内容猜层级时会看漏的地方。
#[test]
fn temporal_presence_is_what_changes_expiry() {
    // 没有时间锚：trait 永不过期，plan 走 30 天默认窗口。
    assert_eq!(expires_of(MemoryKind::Trait, "对方打算看电影"), None);
    assert!(expires_of(MemoryKind::Plan, "对方打算看电影").is_some());

    // 有了时间锚：连 trait 也不再是永久记忆。
    assert!(expires_of(MemoryKind::Trait, "对方下周打算看电影").is_some());
}

#[test]
fn cosine_distance_maps_to_similarity() {
    assert!((similarity(0.0) - 1.0).abs() < 1e-6);
    assert!((similarity(0.21) - 0.79).abs() < 1e-6);
    assert!((similarity(1.0) - 0.0).abs() < 1e-6);
}

/// 去重阈值必须严于召回阈值：去重是"两条说同一件事"的判定，比"够相关可召回"
/// 更强。否则召回时看着毫不相干的两条，写入时会被判成重复而互相吞掉。
#[test]
fn dedup_threshold_is_stricter_than_recall_threshold() {
    let cfg = Config::get();
    assert!(
        cfg.memory_dedup_min_similarity > cfg.memory_recall_min_similarity,
        "去重阈值 {} 应严于召回阈值 {}",
        cfg.memory_dedup_min_similarity,
        cfg.memory_recall_min_similarity
    );
}

#[test]
fn validate_query_rejects_blank() {
    assert!(validate_query("").is_err());
    assert!(validate_query("   ").is_err());
    assert!(validate_query("上海").is_ok());
}
