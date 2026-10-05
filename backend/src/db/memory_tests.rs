//! `db/memory.rs` 向量检索测试：JOIN 取行、过期过滤、余弦距离排序、k 上限、原地改写。

use crate::config::Config;
use crate::db::Database;

use super::*;

/// 库里向量表的维度（由 Config 决定，`Database::in_memory` 建表时写入）。
fn dim() -> usize {
    Config::get().embedding_dim
}

/// 与 query `[1,0,…]` 的**余弦相似度恰为 `sim`** 的向量：`[sim, √(1−sim²), 0…]`。
///
/// 向量表已切到 cosine，所以排序要按相似度设计，不能再用正交向量硬凑距离差——
/// 正交向量在余弦下距离全都是 1，排不出确定的顺序。
fn similar_vector(sim: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; dim()];
    v[0] = sim;
    v[1] = (1.0 - sim * sim).max(0.0).sqrt();
    v
}

/// 造一条真实消息并返回其 id：`memories.message_id` 有外键，填假 id 会插入失败。
fn source_message(db: &Database) -> String {
    let sid = crate::db::session::create(db, "会话").unwrap().id;
    crate::db::message::create(db, &sid, "user", "来源消息")
        .unwrap()
        .id
}

/// 记忆 + 关联消息 + 向量，全套落库。
fn seed(db: &Database, content: &str, expires_at: Option<&str>, embedding: &[f32]) -> String {
    let message_id = source_message(db);
    let m = Memory::new(
        content.to_string(),
        MemoryKind::Trait,
        Some(message_id.clone()),
        expires_at.map(str::to_string),
    );
    create(db, &m, embedding).unwrap();
    m.id
}

/// 相似度 1.0 / 0.8 / 0.5 的三条未过期记忆，外加一条相似度 0.85 的**已过期**记忆。
/// query 取 `[1,0,…]` 时余弦距离序：最近(0) < 已过期(0.15) < 次近(0.2) < 最远(0.5)。
fn seeded() -> Database {
    let db = Database::in_memory().unwrap();
    seed(&db, "最近", None, &similar_vector(1.0));
    seed(&db, "次近", None, &similar_vector(0.8));
    seed(&db, "最远", None, &similar_vector(0.5));
    seed(
        &db,
        "已过期",
        Some("2000-01-01T00:00:00+00:00"),
        &similar_vector(0.85),
    );
    db
}

fn contents(hits: &[(Memory, f32)]) -> Vec<&str> {
    hits.iter().map(|(m, _)| m.content.as_str()).collect()
}

#[test]
fn search_returns_full_memory_rows_from_single_query() {
    let db = seeded();
    let hits = search_similar(&db, &similar_vector(1.0), 5).unwrap();

    assert_eq!(hits.len(), 3);
    let (m, distance) = &hits[0];
    assert_eq!(m.content, "最近");
    assert!(*distance < 1e-6, "自身余弦距离应为 0，实际 {distance}");

    // JOIN 必须把整行带出来：逐字段核对，不能有 NULL / 错位。
    assert_eq!(m.memory_type, MemoryKind::Trait);
    assert!(m.message_id.is_some(), "来源消息 id 应被带出");
    assert!(m.expires_at.is_none());
    assert!(!m.id.is_empty());
    assert!(!m.created_at.is_empty());
    assert!(!m.updated_at.is_empty());

    // 与按 id 单查的路径结果一致。
    let fetched = get(&db, &m.id).unwrap();
    assert_eq!(fetched.content, m.content);
    assert_eq!(fetched.created_at, m.created_at);
    assert_eq!(fetched.updated_at, m.updated_at);
}

#[test]
fn search_orders_by_cosine_distance_ascending() {
    let db = seeded();
    let hits = search_similar(&db, &similar_vector(1.0), 5).unwrap();
    assert_eq!(contents(&hits), vec!["最近", "次近", "最远"]);

    let distances: Vec<f32> = hits.iter().map(|(_, d)| *d).collect();
    assert!(
        distances.windows(2).all(|w| w[0] <= w[1]),
        "距离应升序：{distances:?}"
    );
    assert!(
        (distances[0] - 0.0).abs() < 1e-6 && (distances[1] - 0.2).abs() < 1e-6,
        "余弦距离应等于 1 - 相似度：{distances:?}"
    );
}

#[test]
fn search_excludes_expired_memories() {
    let db = seeded();
    let hits = search_similar(&db, &similar_vector(1.0), 10).unwrap();
    assert!(!contents(&hits).contains(&"已过期"), "过期记忆不应被召回");
}

#[test]
fn search_respects_k_limit() {
    let db = Database::in_memory().unwrap();
    seed(&db, "最近", None, &similar_vector(1.0));
    seed(&db, "次近", None, &similar_vector(0.8));
    seed(&db, "最远", None, &similar_vector(0.5));

    let hits = search_similar(&db, &similar_vector(1.0), 2).unwrap();
    assert_eq!(hits.len(), 2, "k 应限制返回条数");
    assert_eq!(contents(&hits), vec!["最近", "次近"]);
}

/// `k` 约束的是 vec0 的候选数，过期过滤发生在外层——所以过期条目挤占 top-k 时，
/// 最终结果会少于 k。这是既有语义（早先的实现同样如此），记下来免得被当成 bug。
#[test]
fn k_bounds_vector_candidates_not_final_rows() {
    let db = seeded();
    let hits = search_similar(&db, &similar_vector(1.0), 2).unwrap();
    assert_eq!(
        contents(&hits),
        vec!["最近"],
        "top-2 里的过期条目被外层滤掉"
    );
}

#[test]
fn search_skips_vectors_without_matching_memory() {
    let db = Database::in_memory().unwrap();
    // 向量表里留一条没有对应 memories 行的孤儿记录（外键不覆盖 vec0 虚表）。
    db.conn()
        .execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('ghost', ?1)",
            rusqlite::params![to_byte_array(&similar_vector(1.0)).as_slice()],
        )
        .unwrap();
    seed(&db, "真实", None, &similar_vector(1.0));

    let hits = search_similar(&db, &similar_vector(1.0), 5).unwrap();
    assert_eq!(contents(&hits), vec!["真实"], "孤儿向量行应被 JOIN 滤掉");
}

/// 原地改写：id / created_at / message_id 保持不变（记忆身份不变），
/// 内容、种类与到期时间被新事实替换，向量跟着重写。
#[test]
fn revise_replaces_content_in_place() {
    let db = seeded();
    let nearest = search_similar(&db, &similar_vector(1.0), 1).unwrap()[0]
        .0
        .id
        .clone();
    let before = get(&db, &nearest).unwrap();
    let old_created = before.created_at.clone();

    revise(
        &db,
        &before.id,
        "已搬到上海",
        MemoryKind::Plan,
        Some("2099-01-01T00:00:00+00:00"),
        &similar_vector(0.6),
    )
    .unwrap();

    let after = get(&db, &before.id).unwrap();
    assert_eq!(after.content, "已搬到上海");
    // 种类必须跟着改：旧实现只写 tier 不写 memory_type，于是"这是一条 plan"
    // 永远留在库里，界面和后续统计都拿不到真实分类。
    assert_eq!(after.memory_type, MemoryKind::Plan);
    assert_eq!(
        after.expires_at.as_deref(),
        Some("2099-01-01T00:00:00+00:00")
    );
    assert_eq!(after.created_at, old_created, "created_at 不应被改写");
    assert_eq!(after.message_id, before.message_id, "来源消息不应丢");

    // 向量已换成新的：与旧 query 的距离不再是 0。
    let hits = search_similar(&db, &similar_vector(1.0), 5).unwrap();
    let me = hits.iter().find(|(m, _)| m.id == before.id).unwrap();
    assert!(
        (me.1 - 0.4).abs() < 1e-5,
        "余弦距离应更新为 1 - 0.6 = 0.4，实际 {}",
        me.1
    );
}

/// 向量表不可用时（embedding 为空）仍要能改文本，只是不动向量。
#[test]
fn revise_without_embedding_skips_vector_write() {
    let db = seeded();
    let id = search_similar(&db, &similar_vector(1.0), 1).unwrap()[0]
        .0
        .id
        .clone();

    revise(&db, &id, "只改文本", MemoryKind::Preference, None, &[]).unwrap();

    assert_eq!(get(&db, &id).unwrap().content, "只改文本");
    assert_eq!(
        get(&db, &id).unwrap().memory_type,
        MemoryKind::Preference,
        "降级路径也必须更新种类，否则纯文本改写会悄悄改掉分类"
    );
    let hits = search_similar(&db, &similar_vector(1.0), 5).unwrap();
    assert!(
        (hits[0].1 - 0.0).abs() < 1e-6,
        "向量没被改动，自身距离仍应为 0"
    );
}
