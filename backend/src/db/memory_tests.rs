//! `db/memory.rs` 向量检索测试：JOIN 取行、过期过滤、距离排序、k 上限。

use crate::config::Config;
use crate::db::Database;

use super::*;

/// 库里向量表的维度（由 Config 决定，`Database::in_memory` 建表时写入）。
fn dim() -> usize {
    Config::get().embedding_dim
}

/// 只有第 `idx` 维非零、值为 `value` 的正交向量：与 query=[1,0,…] 的距离
/// 恰为 |value - 1|，便于按距离排出确定顺序。
fn axis_vector(idx: usize, value: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; dim()];
    v[idx] = value;
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
        "fact".to_string(),
        "core".to_string(),
        Some(message_id.clone()),
        expires_at.map(str::to_string),
    );
    create(db, &m, embedding).unwrap();
    m.id
}

/// 三个正交记忆 + 一个已过期的最近邻，query 取 [1,0,…]。
fn seeded() -> Database {
    let db = Database::in_memory().unwrap();
    seed(&db, "最近", None, &axis_vector(0, 1.0));
    seed(&db, "次近", None, &axis_vector(1, 2.0));
    seed(&db, "最远", None, &axis_vector(2, 3.0));
    seed(
        &db,
        "已过期",
        Some("2000-01-01T00:00:00+00:00"),
        &axis_vector(3, 1.0),
    );
    db
}

fn contents(hits: &[(Memory, f32)]) -> Vec<&str> {
    hits.iter().map(|(m, _)| m.content.as_str()).collect()
}

#[test]
fn search_returns_full_memory_rows_from_single_query() {
    let db = seeded();
    let hits = search_similar(&db, &axis_vector(0, 1.0), 5).unwrap();

    assert_eq!(hits.len(), 3);
    let (m, distance) = &hits[0];
    assert_eq!(m.content, "最近");
    assert!(*distance < 1e-6, "自身距离应为 0，实际 {distance}");

    // JOIN 必须把整行带出来：逐字段核对，不能有 NULL / 错位。
    assert_eq!(m.memory_type, "fact");
    assert_eq!(m.tier, "core");
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
fn search_orders_by_distance_ascending() {
    let db = seeded();
    let hits = search_similar(&db, &axis_vector(0, 1.0), 5).unwrap();
    assert_eq!(contents(&hits), vec!["最近", "次近", "最远"]);

    let distances: Vec<f32> = hits.iter().map(|(_, d)| *d).collect();
    assert!(
        distances.windows(2).all(|w| w[0] <= w[1]),
        "距离应升序：{distances:?}"
    );
}

#[test]
fn search_excludes_expired_memories() {
    let db = seeded();
    let hits = search_similar(&db, &axis_vector(0, 1.0), 10).unwrap();
    assert!(!contents(&hits).contains(&"已过期"), "过期记忆不应被召回");
}

#[test]
fn search_respects_k_limit() {
    let db = Database::in_memory().unwrap();
    seed(&db, "最近", None, &axis_vector(0, 1.0));
    seed(&db, "次近", None, &axis_vector(1, 2.0));
    seed(&db, "最远", None, &axis_vector(2, 3.0));

    let hits = search_similar(&db, &axis_vector(0, 1.0), 2).unwrap();
    assert_eq!(hits.len(), 2, "k 应限制返回条数");
    assert_eq!(contents(&hits), vec!["最近", "次近"]);
}

/// `k` 约束的是 vec0 的候选数，过期过滤发生在外层——所以过期条目挤占 top-k 时，
/// 最终结果会少于 k。这是既有语义（早先的实现同样如此），记下来免得被当成 bug。
#[test]
fn k_bounds_vector_candidates_not_final_rows() {
    let db = seeded();
    // 距离序：最近(0) < 已过期(√2) < 次近(√5) < 最远(√10)。
    let hits = search_similar(&db, &axis_vector(0, 1.0), 2).unwrap();
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
            rusqlite::params![to_byte_array(&axis_vector(0, 1.0)).as_slice()],
        )
        .unwrap();
    seed(&db, "真实", None, &axis_vector(0, 1.0));

    let hits = search_similar(&db, &axis_vector(0, 1.0), 5).unwrap();
    assert_eq!(contents(&hits), vec!["真实"], "孤儿向量行应被 JOIN 滤掉");
}

#[test]
fn byte_array_roundtrips() {
    let original = vec![0.5f32, -1.25, 3.0];
    let restored = from_byte_array(&to_byte_array(&original)).unwrap();
    assert_eq!(restored, original);
}
