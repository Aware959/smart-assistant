//! `core/self_profile.rs` 渲染缓存测试：版本化快照、旧 KV 兼容、命中复用与显式失效。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::*;
use crate::db::identity::IdentityVersion;
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

fn at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn add_version(db: &Arc<Mutex<Database>>, version: i64, snapshot: &str) {
    let guard = crate::db::lock_db(db);
    crate::db::identity::insert_version(
        &guard,
        &IdentityVersion {
            version,
            snapshot: snapshot.to_string(),
            proposal_id: None,
            source: "manual".to_string(),
            reason: "测试".to_string(),
            created_at: at(),
            operator: None,
            rolled_back_from: None,
        },
    )
    .unwrap();
}

#[test]
fn renders_profile_in_insertion_order() {
    let profile = always_stale(db_with(&[("性格", "慢热"), ("人格", "INFP")]));
    assert_eq!(profile.snapshot().text, "性格：慢热；人格：INFP");
}

#[test]
fn unversioned_profile_keeps_legacy_output_and_zero_version() {
    let profile = always_stale(db_with(&[("性格", "慢热"), ("人格", "INFP")]));
    assert_eq!(
        profile.snapshot(),
        ProfileSnapshot {
            version: Some(AgentIdentity::UNVERSIONED),
            text: "性格：慢热；人格：INFP".to_string(),
        }
    );
}

#[test]
fn latest_version_renders_canonical_fields_instead_of_legacy_kv() {
    let db = db_with(&[("性格", "旧值"), ("爱好", "旧书店")]);
    add_version(
        &db,
        1,
        r#"{"interests":"新书店","personality":"慢热","values":"真诚"}"#,
    );
    let profile = always_stale(db);
    assert_eq!(
        profile.snapshot(),
        ProfileSnapshot {
            version: Some(1),
            text: "性格：慢热；价值观：真诚；兴趣：新书店".to_string(),
        }
    );
}

#[test]
fn cache_keeps_the_identity_version_it_rendered() {
    let db = db_with(&[]);
    add_version(&db, 1, r#"{"personality":"旧版"}"#);
    let profile = sticky(db.clone());
    assert_eq!(profile.snapshot().version, Some(1));

    add_version(&db, 2, r#"{"personality":"新版"}"#);
    let cached = profile.snapshot();
    assert_eq!(
        cached,
        ProfileSnapshot {
            version: Some(1),
            text: "性格：旧版".to_string(),
        },
        "TTL 内应复用同一版快照，而不是只缓存文本"
    );
}

#[test]
fn invalidate_picks_up_a_new_identity_version() {
    let db = db_with(&[]);
    add_version(&db, 1, r#"{"personality":"旧版"}"#);
    let profile = sticky(db.clone());
    assert_eq!(profile.snapshot().version, Some(1));

    add_version(&db, 2, r#"{"personality":"新版"}"#);
    profile.invalidate();
    assert_eq!(
        profile.snapshot(),
        ProfileSnapshot {
            version: Some(2),
            text: "性格：新版".to_string(),
        }
    );
}

#[test]
fn malformed_latest_snapshot_falls_back_to_legacy_profile() {
    let db = db_with(&[("性格", "旧值"), ("自定义", "保留")]);
    add_version(&db, 1, r#"{"personality":42}"#);
    let profile = always_stale(db);
    assert_eq!(
        profile.snapshot(),
        ProfileSnapshot {
            version: None,
            text: "性格：旧值；自定义：保留".to_string(),
        },
        "损坏快照不应让用户丢失旧人设"
    );
}

#[test]
fn empty_version_snapshot_renders_empty_text() {
    let db = db_with(&[("性格", "旧值")]);
    add_version(&db, 1, "{}");
    let profile = always_stale(db);
    assert_eq!(
        profile.snapshot(),
        ProfileSnapshot {
            version: Some(1),
            text: String::new(),
        }
    );
}

#[test]
fn empty_table_renders_empty_string() {
    let profile = always_stale(db_with(&[]));
    assert_eq!(profile.snapshot().text, "");
}

#[test]
fn repeated_calls_within_ttl_return_same_text() {
    let profile = sticky(db_with(&[("性格", "慢热")]));
    let first = profile.snapshot();
    let second = profile.snapshot();
    assert_eq!(first, second);
    assert_eq!(second.text, "性格：慢热");
}

#[test]
fn cache_is_reused_without_rereading_db() {
    let db = db_with(&[("性格", "旧值")]);
    let profile = sticky(db.clone());
    assert_eq!(profile.snapshot().text, "性格：旧值");

    // TTL 内直接改库：应返回缓存里的旧值——这正是"不持锁读 DB"省下来的代价。
    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "性格", "新值").unwrap();
    }
    assert_eq!(
        profile.snapshot().text,
        "性格：旧值",
        "TTL 内应命中缓存，不回读 DB"
    );
}

#[test]
fn invalidate_forces_next_read_within_ttl() {
    let db = db_with(&[("性格", "旧值")]);
    let profile = sticky(db.clone());
    assert_eq!(profile.snapshot().text, "性格：旧值");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "性格", "新值").unwrap();
    }
    profile.invalidate();
    assert_eq!(
        profile.snapshot().text,
        "性格：新值",
        "显式失效后应立即重读，不能等 TTL"
    );
}

#[test]
fn invalidate_is_idempotent_on_empty_cache() {
    let profile = sticky(db_with(&[]));
    profile.invalidate();
    assert_eq!(profile.snapshot().text, "");
}

#[test]
fn expired_cache_rereads_db() {
    let db = db_with(&[("性格", "旧值")]);
    let profile = always_stale(db.clone());
    assert_eq!(profile.snapshot().text, "性格：旧值");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "性格", "新值").unwrap();
    }
    assert_eq!(profile.snapshot().text, "性格：新值", "过期后应重新渲染");
}

#[test]
fn added_key_appears_after_expiry() {
    let db = db_with(&[("性格", "慢热")]);
    let profile = always_stale(db.clone());
    assert_eq!(profile.snapshot().text, "性格：慢热");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "爱好", "旧书店").unwrap();
    }
    assert_eq!(profile.snapshot().text, "性格：慢热；爱好：旧书店");
}

#[test]
fn deleted_key_disappears_after_expiry() {
    let db = db_with(&[("性格", "慢热"), ("爱好", "旧书店")]);
    let profile = always_stale(db.clone());
    assert_eq!(profile.snapshot().text, "性格：慢热；爱好：旧书店");

    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::delete(guard.conn(), "爱好").unwrap();
    }
    assert_eq!(profile.snapshot().text, "性格：慢热");
}
