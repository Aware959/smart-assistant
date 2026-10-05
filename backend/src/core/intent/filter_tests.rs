use super::*;
use crate::db::session;
use crate::db::Database;

fn add_user(db: &Database, channel: &str, external_id: &str) -> String {
    let s = session::create(db, "").unwrap();
    let now = Utc::now().to_rfc3339();
    db.conn()
        .execute(
            "INSERT INTO channel_sessions (channel, external_id, session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![channel, external_id, s.id, now, now],
        )
        .unwrap();
    s.id
}

#[test]
fn eligible_respects_silence_and_gap() {
    // 默认配置：min_silence_hours=2, max_idle_days=7, min_minutes=45
    let db = Database::in_memory().unwrap();
    let sid = add_user(&db, "telegram", "100");
    let base = Utc::now() - Duration::hours(3);
    crate::db::message::create_at(&db, &sid, "user", "在吗", base).unwrap();
    let rows = crate::db::channel::list_proactive_rows(&db).unwrap();
    let now = Utc::now();
    assert_eq!(filter_eligible(rows.clone(), now).len(), 1);
    crate::db::message::create_at(&db, &sid, "assistant", "在的", now - Duration::minutes(10))
        .unwrap();
    let rows = crate::db::channel::list_proactive_rows(&db).unwrap();
    assert!(filter_eligible(rows, now).is_empty());
}

#[test]
fn user_never_spoke_is_not_candidate() {
    let db = Database::in_memory().unwrap();
    add_user(&db, "telegram", "100");
    let rows = crate::db::channel::list_proactive_rows(&db).unwrap();
    assert!(filter_eligible(rows, Utc::now()).is_empty());
}

/// 每日配额已删：同一天里第 N 次主动不受次数限制，只受静默/间隔门槛约束。
#[test]
fn no_daily_quota_gate() {
    let db = Database::in_memory().unwrap();
    let sid = add_user(&db, "ilink", "o123@im.wechat");
    let now = Utc::now();
    crate::db::message::create_at(&db, &sid, "user", "在吗", now - Duration::hours(3)).unwrap();
    // 当天已主动开口 5 次（间隔都拉得开），仍应保持可开口。
    for i in 0..5 {
        crate::db::message::create_at(
            &db,
            &sid,
            "assistant",
            "主动消息",
            now - Duration::minutes(120 - i * 10),
        )
        .unwrap();
    }
    let rows = crate::db::channel::list_proactive_rows(&db).unwrap();
    assert_eq!(filter_eligible(rows, now).len(), 1);
}
