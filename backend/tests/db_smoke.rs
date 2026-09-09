use smart_assistant::config::Config;
use smart_assistant::db;
use smart_assistant::db::memory as db_memory;
use smart_assistant::db::relation as db_relation;

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
    let m1 = db_memory::Memory::new("关于小明的事实".into(), "user".into(), None);
    db_memory::create(&db, &m1, &v).unwrap();

    let m2 = db_memory::Memory::new("另一个记忆".into(), "user".into(), None);
    let v2: Vec<f32> = vector(d, d);
    db_memory::create(&db, &m2, &v2).unwrap();

    let memories = db_memory::list(&db, 10).unwrap();
    assert_eq!(memories.len(), 2);

    // 查询与 m1 相似的向量
    let hits = db_memory::search_similar(&db, &v, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.id, m1.id);
}

#[test]
fn entity_and_relation_ingest() {
    let db = db::Database::in_memory().unwrap();

    let src = db_relation::ensure_entity(&db, "小明", "person", None).unwrap();
    let tgt = db_relation::ensure_entity(&db, "北京", "place", None).unwrap();

    // 同名去重
    let again = db_relation::ensure_entity(&db, "小明", "person", None).unwrap();
    assert_eq!(src, again);

    db_relation::upsert_relation(&db, &src, &tgt, "生活在", 1.0, None).unwrap();
    db_relation::upsert_relation(&db, &src, &tgt, "生活在", 0.5, None).unwrap();

    let rels = db_relation::list_relations(&db).unwrap();
    assert_eq!(rels.len(), 1);
    assert!((rels[0].weight - 1.5).abs() < f32::EPSILON);

    let by_type = db_relation::list_relations_by_type(&db, "生活在").unwrap();
    assert_eq!(by_type.len(), 1);
}

#[test]
fn update_and_delete_memory() {
    let db = db::Database::in_memory().unwrap();

    let v: Vec<f32> = vector(dim(), 0);
    let m = db_memory::Memory::new("待更新".into(), "user".into(), None);
    db_memory::create(&db, &m, &v).unwrap();

    db_memory::update(&db, &m.id, "已更新", &v).unwrap();
    let updated = db_memory::get(&db, &m.id).unwrap();
    assert_eq!(updated.content, "已更新");

    db_memory::delete(&db, &m.id).unwrap();
    let result = db_memory::list(&db, 10).unwrap();
    assert!(result.is_empty());
}
#[test]
fn parse_llm_output_with_alias_fields() {
    use smart_assistant::extractor::parser::parse_raw;

    let raw = r#"{
  "entities": [
    {"name": "王亮武", "type": "person"},
    {"name": "张三", "entity_type": "person", "from": "x", "to": "y", "relation": "z"}
  ],
  "relations": [
    {"from": "王亮武", "to": "北京", "type": "生活在", "weight": 0.8},
    {"source": "王亮武", "target": "公司", "relation": "工作于"}
  ], "extra": 1}"#;
    let result = parse_raw(raw).unwrap();
    assert_eq!(result.entities.len(), 2);
    assert_eq!(result.entities[0].name, "王亮武");
    assert_eq!(result.entities[0].entity_type, "person");
    assert_eq!(result.entities[1].entity_type, "person");

    assert_eq!(result.relations.len(), 2);
    assert_eq!(result.relations[0].source, "王亮武");
    assert_eq!(result.relations[0].target, "北京");
    assert_eq!(result.relations[0].relation, "生活在");
    assert!((result.relations[0].weight - 0.8).abs() < f32::EPSILON);

    // 缺少 weight 时回退默认值 1.0
    assert_eq!(result.relations[1].weight, 1.0);
    assert_eq!(result.relations[1].target, "公司");
}

#[test]
fn graph_recall_returns_related_relations() {
    use smart_assistant::db::relation as db_relation;
    use smart_assistant::extractor::parser::{ExtractedEntity, ExtractionResult};
    use smart_assistant::memory::graph;

    let db = db::Database::in_memory().unwrap();

    let 小明 = db_relation::ensure_entity(&db, "小明", "person", None).unwrap();
    let 北京 = db_relation::ensure_entity(&db, "北京", "place", None).unwrap();
    let 字节 = db_relation::ensure_entity(&db, "字节", "organization", None).unwrap();
    db_relation::upsert_relation(&db, &小明, &北京, "生活在", 1.0, None).unwrap();
    db_relation::upsert_relation(&db, &小明, &字节, "工作于", 0.9, None).unwrap();
    db_relation::upsert_relation(&db, &北京, &字节, "无关联", 0.1, None).unwrap();

let extraction = ExtractionResult {
        entities: vec![ExtractedEntity { name: "小明".into(), entity_type: "person".into() }],
        ..ExtractionResult::empty()
    };

    let lines = graph::recall_graph_context(&db, &extraction).unwrap();
    assert_eq!(lines.len(), 2, "只应召回与小明直接相连的关系");
    assert!(lines.iter().any(|l| l.contains("生活在")));
    assert!(lines.iter().any(|l| l.contains("工作于")));
    assert!(!lines.iter().any(|l| l.contains("无关联")), "无关实体间的关系不应被召回");
}

#[test]
fn graph_recall_caps_relations_per_entity() {
    use smart_assistant::db::relation as db_relation;
    use smart_assistant::extractor::parser::{ExtractedEntity, ExtractionResult};
    use smart_assistant::memory::graph;

    let db = db::Database::in_memory().unwrap();

    let 小明 = db_relation::ensure_entity(&db, "小明", "person", None).unwrap();
    for (name, weight) in [
        ("甲", 0.2),
        ("乙", 1.0),
        ("丙", 0.8),
        ("丁", 0.9),
        ("戊", 0.5),
        ("己", 0.3),
    ] {
        let t = db_relation::ensure_entity(&db, name, "person", None).unwrap();
        db_relation::upsert_relation(&db, &小明, &t, "认识", weight, None).unwrap();
    }

    let extraction = ExtractionResult {
        entities: vec![ExtractedEntity { name: "小明".into(), entity_type: "person".into() }],
        ..ExtractionResult::empty()
    };

    let lines = graph::recall_graph_context(&db, &extraction).unwrap();
    assert_eq!(lines.len(), 3, "每个实体最多带出权重最高的 3 条关系");
    assert!(lines.iter().any(|l| l.contains("乙")), "weight=1.0 的关系应保留");
    assert!(lines.iter().any(|l| l.contains("丁")), "weight=0.9 的关系应保留");
    assert!(lines.iter().any(|l| l.contains("丙")), "weight=0.8 的关系应保留");
    assert!(!lines.iter().any(|l| l.contains("甲")), "低权重关系应被截断");
}

#[test]
fn find_entity_by_name_is_read_only() {
    use smart_assistant::db::relation as db_relation;

    let db = db::Database::in_memory().unwrap();
    let before = db_relation::list_entities(&db).unwrap().len();

    // 查询不存在的名字，不应写入任何实体
let hit = db_relation::find_entity_by_name(&db, "不存在的人").unwrap();
    assert!(hit.is_none());
    assert_eq!(db_relation::list_entities(&db).unwrap().len(), before);
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

    // 最近 3 条：最后是 assistant a9，正序返回 a8, u9, a9
    let recent = message::list_recent(&db, &session.id, 3).unwrap();
    let roles: Vec<&str> = recent.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["assistant", "user", "assistant"]);
    let contents: Vec<&str> = recent.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["a8", "u9", "a9"]);

    // 限制 0 返回空
    assert!(message::list_recent(&db, &session.id, 0).unwrap().is_empty());

    // list_by_session 保持历史正序
    let all = message::list_by_session(&db, &session.id, 100).unwrap();
    assert_eq!(all.len(), 20);
    assert_eq!(all[0].content, "u0");
    assert_eq!(all[19].content, "a9");
}

#[test]
fn delete_entity_removes_its_relations_explicitly() {
    use smart_assistant::db::relation as db_relation;

    let db = db::Database::in_memory().unwrap();

    let 小明 = db_relation::ensure_entity(&db, "小明", "person", None).unwrap();
    let 北京 = db_relation::ensure_entity(&db, "北京", "place", None).unwrap();
    let 字节 = db_relation::ensure_entity(&db, "字节", "organization", None).unwrap();
    db_relation::upsert_relation(&db, &小明, &北京, "生活在", 1.0, None).unwrap();
    db_relation::upsert_relation(&db, &字节, &小明, "雇佣", 0.8, None).unwrap();
    // 与本次删除无关的关系，应保留
    db_relation::upsert_relation(&db, &北京, &字节, "相邻", 0.2, None).unwrap();

    db_relation::delete_entity(&db, &小明).unwrap();

    let rels = db_relation::list_relations(&db).unwrap();
    assert_eq!(rels.len(), 1, "删除实体后其出边/入边应一并删除");
    assert_eq!(rels[0].relation_type, "相邻", "不相关的其它关系应保留");

    let names: Vec<String> = db_relation::list_entities(&db).unwrap().iter().map(|e| e.name.clone()).collect();
    assert!(!names.contains(&"小明".to_string()));
    assert!(names.contains(&"北京".to_string()));
    assert!(names.contains(&"字节".to_string()));
}

#[test]
fn delete_memory_removes_its_entities_and_relations() {
    use smart_assistant::db::memory as db_memory;
    use smart_assistant::db::relation as db_relation;

    let db = db::Database::in_memory().unwrap();

    let m = db_memory::Memory::new("小明生活在北京".into(), "fact".into(), None);
    db_memory::create(&db, &m, &vector(dim(), 0)).unwrap();

    let 小明 = db_relation::ensure_entity(&db, "小明", "person", Some(&m.id)).unwrap();
    db_relation::upsert_relation(&db, &小明, &小明, "是记忆主体", 1.0, Some(&m.id)).unwrap();

    db_memory::delete(&db, &m.id).unwrap();

    assert!(db_relation::list_relations(&db).unwrap().is_empty(), "记忆派生的关系应随记忆删除");
    assert!(db_relation::list_entities(&db).unwrap().is_empty(), "记忆派生的实体应随记忆删除");
}

#[test]
fn services_layer_roundtrip() {
    use smart_assistant::db::relation as db_relation;
    use smart_assistant::services;

    let db = db::Database::in_memory().unwrap();

    // 会话/消息门面
    let sid = services::create_session(&db, "svc").unwrap();
    let sessions = services::list_sessions(&db, 10).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, sid);
    assert_eq!(services::list_messages(&db, &sid, 10).unwrap().len(), 0);

    // 注入一条记忆 + 实体/关系，验证记忆与图谱门面
    let d = dim();
    let m = db_memory::Memory::new("小明是产品的负责人".into(), "fact".into(), None);
    db_memory::create(&db, &m, &vector(d, 10)).unwrap();
    let eid = db_relation::ensure_entity(&db, "小明", "person", None).unwrap();
    db_relation::upsert_relation(&db, &eid, &eid, "负责", 1.0, Some(&m.id)).unwrap();

    let memories = services::list_memories(&db, 10).unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].id, m.id);
    assert_eq!(services::list_entities(&db).unwrap().len(), 1);
    assert_eq!(services::list_relations(&db).unwrap().len(), 1);

    // 删除：实体 → 关系级联
    services::delete_entity(&db, &eid).unwrap();
    assert!(services::list_relations(&db).unwrap().is_empty());
    services::delete_memory(&db, &m.id).unwrap();
    assert!(services::list_memories(&db, 10).unwrap().is_empty());

    services::delete_session(&db, &sid).unwrap();
    assert!(services::list_sessions(&db, 10).unwrap().is_empty());
}
