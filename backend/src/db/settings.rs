//! 运行时配置持久化：值、revision 与逐字段审计。
//!
//! 业务校验和默认值在 `core::settings`；本模块只负责把已经校验过的值
//! 放进同一个 SQLite 事务，保证配置与审计不会半写。

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use crate::db::Database;
use crate::error::{Result, SqlError};

/// 配置持久化适配器。持有现有 DB 句柄，不另开连接。
pub struct SettingsStore {
    db: std::sync::Arc<std::sync::Mutex<Database>>,
}

impl SettingsStore {
    pub(crate) fn new(db: std::sync::Arc<std::sync::Mutex<Database>>) -> Self {
        Self { db }
    }

    pub(crate) fn revision(&self) -> Result<i64> {
        revision(&crate::db::lock_db(&self.db))
    }

    pub(crate) fn load_values(&self) -> Result<BTreeMap<String, Value>> {
        load_values(&crate::db::lock_db(&self.db))
    }

    pub(crate) fn apply_changes(
        &self,
        expected_revision: i64,
        changes: &BTreeMap<String, Value>,
        operator: &str,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        apply_changes(
            &crate::db::lock_db(&self.db),
            expected_revision,
            changes,
            operator,
            now,
        )
    }

    pub(crate) fn list_audit(&self, limit: usize) -> Result<Vec<SettingsAudit>> {
        list_audit(&crate::db::lock_db(&self.db), limit)
    }
}

/// 一次配置写入的审计记录。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingsAudit {
    pub revision: i64,
    pub key: String,
    pub before_json: Option<String>,
    pub after_json: String,
    pub operator: String,
    pub created_at: DateTime<Utc>,
}

/// 当前落库 revision；没有任何变更时返回 0。
pub(crate) fn revision(db: &Database) -> Result<i64> {
    Ok(db.conn().query_row(
        "SELECT COALESCE(MAX(revision), 0) FROM app_settings_revisions",
        [],
        |row| row.get(0),
    )?)
}

/// 读取全部配置值。
pub(crate) fn load_values(db: &Database) -> Result<BTreeMap<String, Value>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT key, value_json FROM app_settings")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (key, raw) = row?;
        let value = serde_json::from_str(&raw)?;
        out.insert(key, value);
    }
    Ok(out)
}

/// 在预期 revision 下原子写入一批已校验配置。
///
/// 返回新的 revision。预期值过期时返回 [`SqlError::Conflict`]，调用方应映射成
/// HTTP 409；这样两个客户端并发保存时不会静默覆盖。
pub(crate) fn apply_changes(
    db: &Database,
    expected_revision: i64,
    changes: &BTreeMap<String, Value>,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<i64> {
    let tx = db.conn().unchecked_transaction()?;
    let current: i64 = tx.query_row(
        "SELECT COALESCE(MAX(revision), 0) FROM app_settings_revisions",
        [],
        |row| row.get(0),
    )?;
    if current != expected_revision {
        return Err(SqlError::Conflict(format!(
            "配置 revision 已变化（当前 {current}，请求 {expected_revision}）"
        )));
    }

    let changed_keys = changes.keys().cloned().collect::<BTreeSet<_>>();
    tx.execute(
        "INSERT INTO app_settings_revisions (operator, created_at, changed_keys)
         VALUES (?1, ?2, ?3)",
        params![
            operator,
            now.to_rfc3339(),
            serde_json::to_string(&changed_keys)?
        ],
    )?;
    let revision: i64 = tx.last_insert_rowid();

    for (key, after) in changes {
        let before: Option<String> = tx
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        let after_json = serde_json::to_string(after)?;
        tx.execute(
            "INSERT INTO app_settings (key, value_json, updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET
                 value_json = excluded.value_json,
                 updated_at = excluded.updated_at,
                 updated_by = excluded.updated_by",
            params![key, after_json, now.to_rfc3339(), operator],
        )?;
        tx.execute(
            "INSERT INTO app_settings_audit
                 (revision, key, before_json, after_json, operator, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision,
                key,
                before,
                after_json,
                operator,
                now.to_rfc3339()
            ],
        )?;
    }
    tx.commit()?;
    Ok(revision)
}

/// 读取审计历史（新到旧）。
pub(crate) fn list_audit(db: &Database, limit: usize) -> Result<Vec<SettingsAudit>> {
    let mut stmt = db.conn().prepare(
        "SELECT revision, key, before_json, after_json, operator, created_at
         FROM app_settings_audit
         ORDER BY revision DESC, key ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (revision, key, before_json, after_json, operator, created_at) = row?;
        out.push(SettingsAudit {
            revision,
            key,
            before_json,
            after_json,
            operator,
            created_at: DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| SqlError::Config(format!("配置审计时间无效：{e}")))?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(secs)
    }

    #[test]
    fn apply_changes_is_atomic_and_writes_audit() {
        let db = Database::in_memory().unwrap();
        let changes = BTreeMap::from([
            ("a".to_string(), serde_json::json!(1)),
            ("b".to_string(), serde_json::json!("x")),
        ]);
        let revision = apply_changes(&db, 0, &changes, "test", at(0)).unwrap();
        assert_eq!(revision, 1);
        assert_eq!(load_values(&db).unwrap(), changes);
        let audit = list_audit(&db, 10).unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[0].revision, 1);
        assert!(audit[0].before_json.is_none());
    }

    #[test]
    fn stale_revision_does_not_write_any_row() {
        let db = Database::in_memory().unwrap();
        let first = BTreeMap::from([("a".to_string(), serde_json::json!(1))]);
        apply_changes(&db, 0, &first, "first", at(0)).unwrap();

        let stale = BTreeMap::from([("b".to_string(), serde_json::json!(2))]);
        let error = apply_changes(&db, 0, &stale, "second", at(1)).unwrap_err();
        assert!(matches!(error, SqlError::Conflict(_)));
        assert_eq!(revision(&db).unwrap(), 1);
        assert!(!load_values(&db).unwrap().contains_key("b"));
    }

    #[test]
    fn audit_records_previous_value_on_overwrite() {
        let db = Database::in_memory().unwrap();
        apply_changes(
            &db,
            0,
            &BTreeMap::from([("a".to_string(), serde_json::json!(1))]),
            "first",
            at(0),
        )
        .unwrap();
        apply_changes(
            &db,
            1,
            &BTreeMap::from([("a".to_string(), serde_json::json!(2))]),
            "second",
            at(1),
        )
        .unwrap();
        let audit = list_audit(&db, 1).unwrap();
        assert_eq!(audit[0].before_json.as_deref(), Some("1"));
        assert_eq!(audit[0].after_json, "2");
    }
}
