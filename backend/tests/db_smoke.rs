use smart_assistant::config::Config;
use smart_assistant::db;
use smart_assistant::db::memory as db_memory;

/// 按当前配置的 embedding 维度生成测试向量。
fn vector(dim: usize, start: usize) -> Vec<f32> {
    (start..start + dim).map(|i| i as f32).collect()
}

fn dim() -> usize {
    let _ = Config::init_from_env();
    Config::get().embedding_dim
}

#[test]
fn sqlite_vec_extension_available() {
    let conn = db::Database::in_memory().expect("open in-memory db");
    let version: String = conn
        .conn()
        .query_row("SELECT vec_version()", [], |r| r.get(0))
        .expect("vec_version() should work");
    assert!(version.starts_with('v'), "expected vec version, got {version}");
}

#[test]
fn store_and_search_memory() {
    let db = db::Database::in_memory().unwrap();

    let d = dim();
    let v: Vec<f32> = vector(d, 0);
    let m1 = db_memory::Memory::new("关于小明的事实".into(), "user".into(), "core".into(), None, None);
    db_memory::create(&db, &m1, &v).unwrap();

    let m2 = db_memory::Memory::new("另一个记忆".into(), "user".into(), "core".into(), None, None);
    let v2: Vec<f32> = vector(d, d);
    db_memory::create(&db, &m2, &v2).unwrap();

    let memories = db_memory::list(&db, 10).unwrap();
    assert_eq!(memories.len(), 2);

    let hits = db_memory::search_similar(&db, &v, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.id, m1.id);
}

#[test]
fn update_and_delete_memory() {
    let db = db::Database::in_memory().unwrap();

    let v: Vec<f32> = vector(dim(), 0);
    let m = db_memory::Memory::new("待更新".into(), "user".into(), "core".into(), None, None);
    db_memory::create(&db, &m, &v).unwrap();

    db_memory::update(&db, &m.id, "已更新", &v).unwrap();
    let updated = db_memory::get(&db, &m.id).unwrap();
    assert_eq!(updated.content, "已更新");

    db_memory::delete(&db, &m.id).unwrap();
    let result = db_memory::list(&db, 10).unwrap();
    assert!(result.is_empty());
}

#[test]
fn memory_extraction_falls_back_to_raw_text() {
    use smart_assistant::memory::extraction::MemoryExtraction;

    // 事实化内容缺失（未生成或生成失败）时，落库文本应回退到用户原文。
    let extraction = MemoryExtraction {
        is_memory: true,
        memory_content: None,
        memory_type: "fact".to_string(),
        tier: "core".to_string(),
        relation: "neutral".to_string(),
    };
    assert_eq!(extraction.content_or("我最喜欢蓝色了"), "我最喜欢蓝色了");
}

#[test]
fn message_list_recent_orders_and_limits() {
    use smart_assistant::db::message;

    let db = db::Database::in_memory().unwrap();
    let session = db::session::create(&db, "t").unwrap();

    for i in 0..10 {
        message::create(&db, &session.id, "user", &format!("u{i}")).unwrap();
        message::create(&db, &session.id, "assistant", &format!("a{i}")).unwrap();
    }

    let recent = message::list_recent(&db, &session.id, 3).unwrap();
    let roles: Vec<&str> = recent.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["assistant", "user", "assistant"]);
    let contents: Vec<&str> = recent.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["a8", "u9", "a9"]);

    assert!(message::list_recent(&db, &session.id, 0).unwrap().is_empty());

    let all = message::list_by_session(&db, &session.id, 100).unwrap();
    assert_eq!(all.len(), 20);
    assert_eq!(all[0].content, "u0");
    assert_eq!(all[19].content, "a9");
}

#[test]
fn channel_session_mapping_is_stable_and_resettable() {
    use smart_assistant::db::channel;

    let db = db::Database::in_memory().unwrap();

    // 同一外部对话 → 同一 session
    let s1 = channel::get_or_create_session(&db, "telegram", "1001").unwrap();
    let s2 = channel::get_or_create_session(&db, "telegram", "1001").unwrap();
    assert_eq!(s1.id, s2.id);

    // 不同外部对话 → 不同 session
    let s3 = channel::get_or_create_session(&db, "telegram", "1002").unwrap();
    assert_ne!(s1.id, s3.id);

    // 不同渠道互不干扰
    let s4 = channel::get_or_create_session(&db, "other", "1001").unwrap();
    assert_ne!(s1.id, s4.id);

    // reset 后换新 session
    let s5 = channel::reset_session(&db, "telegram", "1001").unwrap();
    assert_ne!(s1.id, s5.id);
    let s6 = channel::get_or_create_session(&db, "telegram", "1001").unwrap();
    assert_eq!(s5.id, s6.id);
}

#[test]
fn services_layer_roundtrip() {
    use smart_assistant::services;

    let db = db::Database::in_memory().unwrap();

    let sid = services::create_session(&db, "svc").unwrap();
    let sessions = services::list_sessions(&db, 10).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, sid);
    assert_eq!(services::list_messages(&db, &sid, 10).unwrap().len(), 0);

    let d = dim();
    let m = db_memory::Memory::new("小明是产品的负责人".into(), "fact".into(), "core".into(), None, None);
    db_memory::create(&db, &m, &vector(d, 10)).unwrap();

    let memories = services::list_memories(&db, 10).unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].id, m.id);

    services::delete_memory(&db, &m.id).unwrap();
    assert!(services::list_memories(&db, 10).unwrap().is_empty());

    services::delete_session(&db, &sid).unwrap();
    assert!(services::list_sessions(&db, 10).unwrap().is_empty());
}
