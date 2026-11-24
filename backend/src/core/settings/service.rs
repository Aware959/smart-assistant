//! 共享配置服务：读取、校验、原子更新、审计与运行时快照发布。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::core::settings::contract::{schema, RuntimeSettings, SettingsSchemaItem};
use crate::core::settings::runtime::SettingsRuntime;
use crate::db::settings::SettingsStore;
use crate::db::Database;
use crate::error::Result;

/// 配置读取视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsView {
    pub revision: i64,
    pub values: BTreeMap<String, Value>,
}

/// 配置更新结果；`restart_required` 目前为空，但契约保留给后续 restart-only 项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsUpdate {
    pub revision: i64,
    pub changed: Vec<String>,
    pub restart_required: Vec<String>,
    pub values: BTreeMap<String, Value>,
}

/// 对外配置审计视图。控制接口只依赖 core 的 DTO，不直接引用 db 层类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsAuditEntry {
    pub revision: i64,
    pub key: String,
    pub before_json: Option<String>,
    pub after_json: String,
    pub operator: String,
    pub created_at: DateTime<Utc>,
}

pub struct SettingsService {
    store: SettingsStore,
    runtime: Arc<SettingsRuntime>,
}

impl SettingsService {
    pub(crate) fn new(db: Arc<Mutex<Database>>) -> Result<Self> {
        let bootstrap = RuntimeSettings::from_bootstrap(Config::get());
        let store = SettingsStore::new(db);
        let persisted = store.load_values()?;
        let current = apply_persisted(&bootstrap, &persisted)?;
        let revision = store.revision()?;
        Ok(Self {
            store,
            runtime: Arc::new(SettingsRuntime::new(current, revision)),
        })
    }

    /// 首次把 `.env` 当前值写进空表；已有任意配置行时不覆盖。
    ///
    /// 迁移只在真实进程装配时调用；测试直接构造 service 不会污染全局环境。
    pub(crate) fn seed_from_bootstrap(&self, now: DateTime<Utc>) -> Result<()> {
        if self.store.revision()? > 0 || !self.store.load_values()?.is_empty() {
            return Ok(());
        }
        let bootstrap = RuntimeSettings::from_bootstrap(Config::get());
        let revision = self
            .store
            .apply_changes(0, &bootstrap.to_map(), "bootstrap:env", now)?;
        self.runtime.replace(bootstrap, revision);
        tracing::info!(revision, "运行时配置已从 .env 初始化");
        Ok(())
    }

    pub fn runtime(&self) -> Arc<SettingsRuntime> {
        self.runtime.clone()
    }

    pub fn current(&self) -> SettingsView {
        SettingsView {
            revision: self.runtime.revision(),
            values: self.runtime.snapshot().to_map(),
        }
    }

    pub fn schema(&self) -> Vec<SettingsSchemaItem> {
        schema(&self.runtime.snapshot())
    }

    /// 先校验整批变更，再在单个事务中写值、revision 与审计，最后替换运行时快照。
    pub fn update(
        &self,
        changes: BTreeMap<String, Value>,
        expected_revision: i64,
        operator: &str,
        now: DateTime<Utc>,
    ) -> Result<SettingsUpdate> {
        if changes.is_empty() {
            return Ok(SettingsUpdate {
                revision: self.runtime.revision(),
                changed: Vec::new(),
                restart_required: Vec::new(),
                values: self.runtime.snapshot().to_map(),
            });
        }
        if operator.trim().is_empty() {
            return Err(crate::error::SqlError::Config(
                "配置更新缺少 operator".to_string(),
            ));
        }
        let (next, normalized) = self.runtime.snapshot().apply(&changes)?;
        let revision = self
            .store
            .apply_changes(expected_revision, &normalized, operator, now)?;
        let changed = normalized.keys().cloned().collect();
        self.runtime.replace(next.clone(), revision);
        tracing::info!(
            revision,
            operator,
            changed = ?normalized.keys().collect::<Vec<_>>(),
            "运行时配置已更新"
        );
        Ok(SettingsUpdate {
            revision,
            changed,
            restart_required: Vec::new(),
            values: next.to_map(),
        })
    }

    /// 只校验不落库，供桌面保存前预检。
    pub fn validate(&self, changes: BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>> {
        let (_, normalized) = self.runtime.snapshot().apply(&changes)?;
        Ok(normalized)
    }

    pub fn audit(&self, limit: usize) -> Result<Vec<SettingsAuditEntry>> {
        Ok(self
            .store
            .list_audit(limit)?
            .into_iter()
            .map(|audit| SettingsAuditEntry {
                revision: audit.revision,
                key: audit.key,
                before_json: audit.before_json,
                after_json: audit.after_json,
                operator: audit.operator,
                created_at: audit.created_at,
            })
            .collect())
    }
}

fn apply_persisted(
    bootstrap: &RuntimeSettings,
    persisted: &BTreeMap<String, Value>,
) -> Result<RuntimeSettings> {
    if persisted.is_empty() {
        return Ok(bootstrap.clone());
    }
    let (current, _) = bootstrap.apply(persisted)?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use chrono::DateTime;
    use serde_json::json;

    use crate::config::Config;
    use crate::core::settings::contract::RuntimeSettings;
    use crate::core::settings::SettingsService;
    use crate::db::Database;
    use crate::error::SqlError;

    fn at(secs: i64) -> DateTime<chrono::Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
            + chrono::Duration::seconds(secs)
    }

    fn service() -> SettingsService {
        SettingsService::new(Arc::new(Mutex::new(Database::in_memory().unwrap()))).unwrap()
    }

    #[test]
    fn empty_database_falls_back_to_bootstrap_values() {
        let service = service();
        let bootstrap = RuntimeSettings::from_bootstrap(Config::get());
        assert_eq!(service.current().values, bootstrap.to_map());
        assert_eq!(service.current().revision, 0);
    }

    #[test]
    fn update_is_atomic_and_advances_revision() {
        let service = service();
        let before = service.current();
        let mut changes = BTreeMap::new();
        changes.insert("intent_enabled".to_string(), json!(true));
        changes.insert("identity_evolution_interval_hours".to_string(), json!(12));

        let updated = service
            .update(changes, before.revision, "test", at(0))
            .unwrap();
        assert_eq!(updated.revision, 1);
        assert_eq!(updated.changed.len(), 2);
        assert_eq!(service.current().values["intent_enabled"], json!(true));
        assert_eq!(
            service.current().values["identity_evolution_interval_hours"],
            json!(12)
        );
        assert_eq!(service.audit(10).unwrap().len(), 2);
    }

    #[test]
    fn stale_revision_is_a_conflict_and_does_not_write() {
        let service = service();
        let mut changes = BTreeMap::new();
        changes.insert("intent_enabled".to_string(), json!(true));
        service.update(changes, 0, "first", at(0)).unwrap();

        let mut stale = BTreeMap::new();
        stale.insert("intent_enabled".to_string(), json!(false));
        let error = service.update(stale, 0, "second", at(1)).unwrap_err();
        assert!(matches!(error, SqlError::Conflict(_)));
        assert_eq!(service.current().revision, 1);
        assert_eq!(service.current().values["intent_enabled"], json!(true));
    }

    #[test]
    fn invalid_batch_does_not_partially_apply() {
        let service = service();
        let before = service.current();
        let mut changes = BTreeMap::new();
        changes.insert("intent_min_minutes".to_string(), json!(1));
        changes.insert("intent_max_minutes".to_string(), json!(0));
        assert!(service
            .update(changes, before.revision, "test", at(0))
            .is_err());
        assert_eq!(service.current().revision, before.revision);
        assert_eq!(service.current().values, before.values);
    }

    #[test]
    fn seed_from_bootstrap_is_idempotent_and_never_overwrites() {
        let service = service();
        service.seed_from_bootstrap(at(0)).unwrap();
        assert_eq!(service.current().revision, 1);
        let first = service.current();
        service.seed_from_bootstrap(at(1)).unwrap();
        assert_eq!(service.current().revision, first.revision);
        assert_eq!(service.current().values, first.values);
    }

    #[test]
    fn audit_records_before_and_after_without_secret_fields() {
        let service = service();
        let mut changes = BTreeMap::new();
        changes.insert("intent_enabled".to_string(), json!(true));
        service.update(changes, 0, "desktop:test", at(0)).unwrap();
        let audit = service.audit(1).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].key, "intent_enabled");
        assert_eq!(audit[0].operator, "desktop:test");
        assert!(audit[0].after_json.contains("true"));
    }
}
