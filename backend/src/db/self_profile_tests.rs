//! `db/self_profile.rs` 读写测试：KV 增删改查与插入序稳定。

use rusqlite::Connection;

use super::*;

fn open_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE self_profile (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )
    .unwrap();
    conn
}

#[test]
fn set_reads_back_in_insertion_order() {
    let conn = open_conn();
    set(&conn, "性格", "热情、开放").unwrap();
    set(&conn, "人格", "INFP").unwrap();
    set(&conn, "爱好", "打游戏、健身、追剧").unwrap();

    let rows = all(&conn).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], ("性格".to_string(), "热情、开放".to_string()));
    assert_eq!(rows[1], ("人格".to_string(), "INFP".to_string()));
    assert_eq!(
        rows[2],
        ("爱好".to_string(), "打游戏、健身、追剧".to_string())
    );
}

#[test]
fn set_overwrites_value_and_touches_updated_at() {
    let conn = open_conn();
    set(&conn, "性格", "热情").unwrap();
    let first: String = conn
        .query_row(
            "SELECT updated_at FROM self_profile WHERE key='性格'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    set(&conn, "性格", "热情、开放").unwrap();

    let rows = all(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "热情、开放");
    let second: String = conn
        .query_row(
            "SELECT updated_at FROM self_profile WHERE key='性格'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(second >= first, "覆盖应刷新 updated_at");
}

#[test]
fn delete_removes_key_quietly() {
    let conn = open_conn();
    set(&conn, "性格", "热情").unwrap();
    delete(&conn, "性格").unwrap();
    assert!(all(&conn).unwrap().is_empty());
    delete(&conn, "不存在的键").unwrap();
    assert!(all(&conn).unwrap().is_empty());
}

#[test]
fn empty_table_reads_back_nothing() {
    let conn = open_conn();
    assert!(all(&conn).unwrap().is_empty());
}
