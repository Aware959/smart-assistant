//! `db/channel.rs` 测试：主动开口时间账本由消息表派生。

use chrono::{Duration, Utc};

use super::*;
use crate::db::message;
use crate::db::session;

/// 时间戳统一取整到秒，避免 RFC3339 亚秒部分让字符串比较与展示对不上。
fn at(base: DateTime<Utc>, secs: i64) -> DateTime<Utc> {
    base + Duration::seconds(secs)
}

/// 账本就是消息时间轴的投影：最近一条 user、最近一条 assistant。
///
/// 这是删掉 `intent_state` 表的依据——它存的每一个值都在这里被重新算出来。
#[test]
fn proactive_rows_derive_timeline_from_messages() {
    let db = Database::in_memory().unwrap();
    let s = session::create(&db, "").unwrap();
    db.conn()
        .execute(
            "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
             VALUES ('telegram', '100', ?1, 'c', 'u')",
            [&s.id],
        )
        .unwrap();

    let base = Utc::now() - Duration::hours(3);
    message::create_at(&db, &s.id, "user", "在吗", at(base, 0)).unwrap();
    message::create_at(&db, &s.id, "assistant", "在的", at(base, 5)).unwrap();
    message::create_at(&db, &s.id, "user", "吃了吗", at(base, 600)).unwrap();

    let rows = list_proactive_rows(&db).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.channel, "telegram");
    assert_eq!(row.external_id, "100");
    assert_eq!(row.session_id, s.id);
    assert_eq!(
        row.last_user_at,
        Some(base + Duration::seconds(600)),
        "最近入站应取最后一条 user"
    );
    assert_eq!(
        row.last_assistant_at,
        Some(base + Duration::seconds(5)),
        "最近外发应取最后一条 assistant"
    );
}

/// 有映射但从未说过话 → 两个时间戳都是 None（上层据此判"不是候选"）。
#[test]
fn proactive_rows_without_messages_have_no_timeline() {
    let db = Database::in_memory().unwrap();
    let s = session::create(&db, "").unwrap();
    db.conn()
        .execute(
            "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
             VALUES ('telegram', '100', ?1, 'c', 'u')",
            [&s.id],
        )
        .unwrap();

    let rows = list_proactive_rows(&db).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].last_user_at, None);
    assert_eq!(rows[0].last_assistant_at, None);
}

/// 消息时间轴必须按**真实发出时刻**记账，而不是接收时刻。
///
/// 离线 5 小时后补收的消息若按接收时刻入库，静默窗口会从 0 起算，主动引擎会误以为
/// 对方刚说过话而压掉本该发生的开口。这是 `messages.created_at` 语义的回归锁。
#[test]
fn proactive_rows_use_real_send_time_not_receive_time() {
    let db = Database::in_memory().unwrap();
    let s = session::create(&db, "").unwrap();
    db.conn()
        .execute(
            "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
             VALUES ('telegram', '100', ?1, 'c', 'u')",
            [&s.id],
        )
        .unwrap();

    let sent_at = Utc::now() - Duration::hours(5);
    message::create_at(&db, &s.id, "user", "在吗", at(sent_at, 0)).unwrap();

    let rows = list_proactive_rows(&db).unwrap();
    assert_eq!(
        rows[0].last_user_at,
        Some(sent_at),
        "账本必须记真实发出时刻，静默窗口才不会被归零"
    );
}

/// 多用户时每个映射的账本各自独立，不能串号。
#[test]
fn proactive_rows_are_per_mapping() {
    let db = Database::in_memory().unwrap();
    let base = Utc::now() - Duration::hours(4);
    for (external, secs) in [("100", 0_i64), ("200", 3600)] {
        let s = session::create(&db, "").unwrap();
        db.conn()
            .execute(
                "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
                 VALUES ('telegram', ?1, ?2, 'c', 'u')",
                rusqlite::params![external, s.id],
            )
            .unwrap();
        message::create_at(&db, &s.id, "user", "在吗", at(base, secs)).unwrap();
    }

    let rows = list_proactive_rows(&db).unwrap();
    assert_eq!(rows.len(), 2);
    let row = |ext: &str| rows.iter().find(|r| r.external_id == ext).unwrap();
    assert_eq!(row("100").last_user_at, Some(base));
    assert_eq!(
        row("200").last_user_at,
        Some(base + Duration::seconds(3600))
    );
    assert!(rows.iter().all(|r| r.last_assistant_at.is_none()));
}
