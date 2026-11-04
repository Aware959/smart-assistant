//! `db/mod.rs` 向量可用性快路径测试：与完整巡检结果一致，且不做全表计数。

use crate::config::Config;

use super::*;

/// `vec_search_usable()` 必须与 `vec_status().search_usable()` 逐场景一致——
/// 前者是热路径的廉价替身，两者语义分叉会让"检索暂停"判断失真。
fn assert_agrees(db: &Database, label: &str) -> bool {
    let full = db.vec_status().unwrap().search_usable();
    let fast = db.vec_search_usable().unwrap();
    assert_eq!(full, fast, "{label}: 快路径与完整巡检结论不一致");
    full
}

#[test]
fn fresh_db_is_searchable() {
    let db = Database::in_memory().unwrap();
    assert!(assert_agrees(&db, "全新库"));
}

#[test]
fn mismatched_dimension_is_not_searchable() {
    let db = Database::in_memory().unwrap();
    assert!(assert_agrees(&db, "重建前"));

    // 换成与配置不同的维度（vec_status 仍能看到 meta 里的旧签名）。
    db.rebuild_vectors(Config::get().embedding_dim + 4, "other-model")
        .unwrap();

    assert!(!assert_agrees(&db, "维度不一致"));
}

#[test]
fn missing_table_is_not_searchable() {
    let db = Database::in_memory().unwrap();
    db.conn()
        .execute_batch("DROP TABLE memory_vectors;")
        .unwrap();
    assert!(!assert_agrees(&db, "表被误删"));
}

#[test]
fn restoring_dimension_makes_it_searchable_again() {
    let db = Database::in_memory().unwrap();
    let configured = Config::get().embedding_dim;
    db.rebuild_vectors(configured + 4, "other-model").unwrap();
    assert!(!assert_agrees(&db, "维度不一致"));

    db.rebuild_vectors(configured, &Config::get().embedding_model)
        .unwrap();
    assert!(assert_agrees(&db, "恢复到配置维度"));
}

/// 目录不存在时应自动创建而不是报错（用户写错了路径也能一键恢复）。
#[test]
fn ensure_parent_writable_creates_missing_dir() {
    let base = std::env::temp_dir()
        .join("zhiwei-test")
        .join(format!("db-probe-{}", std::process::id()));
    let path = base.join("nested").join("smart_assistant.db");
    let _ = std::fs::remove_dir_all(&base);

    Database::ensure_parent_writable(path.to_str().unwrap()).unwrap();
    assert!(base.join("nested").is_dir(), "嵌套目录应被自动创建");
    assert!(!base.join("nested").join(".zhiwei-write-probe").exists());

    let _ = std::fs::remove_dir_all(&base);
}

/// 目录不可写时应返回明确的 Config 错误，而不是让 SQLite 给含糊的 os error。
#[test]
fn ensure_parent_writable_rejects_unwritable_dir() {
    // Windows 下模拟"不可写"最可靠的方式是选一个不存在的盘符。
    let path = "Z:\\nonexistent\\smart_assistant.db";
    let result = Database::ensure_parent_writable(path);
    assert!(result.is_err(), "不存在的盘符应该报错");
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("数据库目录"),
        "错误信息应包含目录路径：{message}"
    );
}
