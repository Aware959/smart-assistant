//! `core/self_profile.rs` 渲染缓存测试：命中复用、过期刷新、读取失败降级。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::db::Database;

/// 长 TTL：连续调用必然命中缓存。
fn sticky(db: Arc<Mutex<Database>>) -> Profile {
    Profile::with_ttl(db, Duration::from_secs(600))
}

/// 零 TTL：每次调用都判定为过期，必然回落到真读 DB。
fn always_stale(db: Arc<Mutex<Database>>) -> Profile {
    Profile::with_ttl(db, Duration::ZERO)
}

fn db_with(profile: &[(&str, &str)]) -> Arc<Mutex<Database>> {
    let db = Arc::new(Mutex::new(Database::in_memory().unwrap()));
    {
        let guard = crate::db::lock_db(&db);
        for (k, v) in profile {
            crate::db::self_profile::set(guard.conn(), k, v).unwrap();
        }
    }
    db
}

#[test]
fn renders_profile_in_insertion_order() {
    let profile = always_stale(db_with(&[("性格", "慢热"), ("人格", "INFP")]));
    assert_eq!(profile.block(), "性格：慢热；人格：INFP");
}

#[test]
fn empty_table_renders_empty_string() {
    let profile = always_stale(db_with(&[]));
    assert_eq!(profile.block(), "");
}

#[test]
fn repeated_calls_within_ttl_return_same_text() {
    let profile = sticky(db_with(&[("性格", "慢热")]));
    let first = profile.block();
    let second = profile.block();
    assert_eq!(first, second);
    assert_eq!(second, "性格：慢热");
}

#[test]
fn cache_is_reused_without_rereading_db() {
    let db = db_with(&[("性格", "旧值")]);
    let profile = sticky(db.clone());
    assert_eq!(profile.block(), "性格：旧值");

    // TTL 内直接改库：应返回缓存里的旧值——这正是"不持锁读 DB"省下来的代价。
    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "性格", "新值").unwrap();
    }
    assert_eq!(profile.block(), "性格：旧值", "TTL 内应命中缓存，不回读 DB");
}

#[test]
fn expired_cache_rereads_db() {
    let db = db_with(&[("性格", "旧值")]);
    let profile = always_stale(db.clone());
    assert_eq!(profile.block(), "性格：旧值");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "性格", "新值").unwrap();
    }
    assert_eq!(profile.block(), "性格：新值", "过期后应重新渲染");
}

#[test]
fn added_key_appears_after_expiry() {
    let db = db_with(&[("性格", "慢热")]);
    let profile = always_stale(db.clone());
    assert_eq!(profile.block(), "性格：慢热");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "爱好", "旧书店").unwrap();
    }
    assert_eq!(profile.block(), "性格：慢热；爱好：旧书店");
}

#[test]
fn deleted_key_disappears_after_expiry() {
    let db = db_with(&[("性格", "慢热"), ("爱好", "旧书店")]);
    let profile = always_stale(db.clone());
    assert_eq!(profile.block(), "性格：慢热；爱好：旧书店");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::delete(guard.conn(), "爱好").unwrap();
    }
    assert_eq!(profile.block(), "性格：慢热");
}
