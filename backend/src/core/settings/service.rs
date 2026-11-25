//! 共享配置服务：读取、校验、原子更新、审计与运行时快照发布。

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::core::settings::contract::{
    schema, RuntimeSettings, SettingsKey, SettingsSchemaItem, SECRET_MASK,
};
use crate::core::settings::runtime::SettingsRuntime;
use crate::db::settings::SettingsStore;
use crate::db::Database;
use crate::error::Result;
use std::sync::{Arc, Mutex};

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
    /// 只播种**策略键**（记忆 / 主动开口 / 身份演化）。接入键（大模型 / 向量）不播种：
    /// 它们是用户会在 `.env` 与面板之间来回挪的东西，一播种就固化成"数据库说了算"，
    /// 改回 `.env` 就不生效了——那正是面板要消除的困惑。
    ///
    /// 迁移只在真实进程装配时调用；测试直接构造 service 不会污染全局环境。
    pub(crate) fn seed_from_bootstrap(&self, now: DateTime<Utc>) -> Result<()> {
        if self.store.revision()? > 0 || !self.store.load_values()?.is_empty() {
            return Ok(());
        }
        let bootstrap = RuntimeSettings::from_bootstrap(Config::get());
        let seeded: BTreeMap<String, Value> = bootstrap
            .to_map()
            .into_iter()
            .filter(|(key, _)| {
                SettingsKey::parse(key)
                    .map(|parsed| parsed.is_policy())
                    .unwrap_or(false)
            })
            .collect();
        let revision =
            self.store
                .apply_changes(0, &seeded, &secret_keys(), "bootstrap:env", now)?;
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
            values: self.runtime.snapshot().to_public_map(),
        }
    }

    pub fn schema(&self) -> Vec<SettingsSchemaItem> {
        schema(&self.runtime.snapshot())
    }

    /// 先校验整批变更，再在单个事务中写值、revision 与审计，最后替换运行时快照。
    ///
    /// 密钥类的变更若传的是占位符 [`SECRET_MASK`]，视为"这一项没改"——面板每次
    /// 保存都会把看到的占位符原样回传，把它当新值写进去会把真密钥冲掉。
    pub fn update(
        &self,
        changes: BTreeMap<String, Value>,
        expected_revision: i64,
        operator: &str,
        now: DateTime<Utc>,
    ) -> Result<SettingsUpdate> {
        let changes = without_masked_secrets(changes);
        if changes.is_empty() {
            return Ok(SettingsUpdate {
                revision: self.runtime.revision(),
                changed: Vec::new(),
                restart_required: Vec::new(),
                values: self.runtime.snapshot().to_public_map(),
            });
        }
        if operator.trim().is_empty() {
            return Err(crate::error::SqlError::Config(
                "配置更新缺少 operator".to_string(),
            ));
        }
        let (next, normalized) = self.runtime.snapshot().apply(&changes)?;
        let revision = self.store.apply_changes(
            expected_revision,
            &normalized,
            &secret_keys(),
            operator,
            now,
        )?;
        let changed = normalized.keys().cloned().collect();
        self.runtime.replace(next.clone(), revision);
        tracing::info!(
            revision,
            operator,
            changed = ?normalized.keys().collect::<Vec<_>>(),
            "运行时配置已更新"
        );
        // 改了要重启才生效的键，明确告诉调用方，免得以为保存了就生效。
        let restart_required = normalized
            .keys()
            .filter_map(|key| SettingsKey::parse(key).ok())
            .filter(|key| key.restart_required())
            .map(|key| key.as_str().to_string())
            .collect();
        Ok(SettingsUpdate {
            revision,
            changed,
            restart_required,
            values: next.to_public_map(),
        })
    }

    /// 只校验不落库，供桌面保存前预检。
    pub fn validate(&self, changes: BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>> {
        let changes = without_masked_secrets(changes);
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

/// 把库里持久化的值叠加到 `.env` 缺省值上。
///
/// **未知键跳过并记警告**，不让它把整个服务卡在起不来：配置键集会随版本增减，
/// 库里留着一个老键不该是致命的——它只是不再被读取。
fn apply_persisted(
    bootstrap: &RuntimeSettings,
    persisted: &BTreeMap<String, Value>,
) -> Result<RuntimeSettings> {
    if persisted.is_empty() {
        return Ok(bootstrap.clone());
    }
    let known: BTreeMap<String, Value> = persisted
        .iter()
        .filter(|(key, _)| match SettingsKey::parse(key) {
            Ok(_) => true,
            Err(_) => {
                tracing::warn!(key, "配置表里有当前版本不认识的键，已忽略");
                false
            }
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if known.is_empty() {
        return Ok(bootstrap.clone());
    }
    let (current, _) = bootstrap.apply(&known)?;
    Ok(current)
}

/// 剔除"占位符原样回传"的密钥项。
///
/// 面板每次保存都会把看到的 `********` 一并回传；把它当新值写进去会把真密钥冲掉。
/// 只对**密钥类键**生效——普通键的值恰好等于 `********` 时是用户的本意，不该被吞掉。
fn without_masked_secrets(changes: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    changes
        .into_iter()
        .filter(|(key, value)| {
            if value.as_str() != Some(SECRET_MASK) {
                return true;
            }
            !SettingsKey::parse(key)
                .map(|parsed| parsed.is_secret())
                .unwrap_or(false)
        })
        .collect()
}

/// 审计表里要脱敏的键集合。
///
/// 审计会长期留在数据库里，明文密钥不该进去。脱敏后只留"改过"这个事实。
pub(crate) fn secret_keys() -> BTreeSet<String> {
    SettingsKey::ALL
        .into_iter()
        .filter(|key| key.is_secret())
        .map(|key| key.as_str().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};

    use chrono::DateTime;
    use serde_json::json;

    use crate::config::Config;
    use crate::core::settings::contract::{RuntimeSettings, SECRET_MASK};
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

    #[test]
    fn 密钥占位符原样回传视为未修改() {
        let service = service();
        let mut changes = BTreeMap::new();
        changes.insert("llm_api_key".to_string(), json!("sk-first"));
        service.update(changes, 0, "desktop:test", at(0)).unwrap();

        // 面板看到的是占位符，回传它不该把真密钥冲掉。
        let revision = service.current().revision;
        let mut changes = BTreeMap::new();
        changes.insert("llm_api_key".to_string(), json!(SECRET_MASK));
        let updated = service
            .update(changes, revision, "desktop:test", at(1))
            .unwrap();
        assert!(updated.changed.is_empty(), "占位符不应算作变更");
        assert_eq!(
            service.runtime().snapshot().llm.api_key,
            "sk-first",
            "真密钥应原样保留"
        );
    }

    #[test]
    fn 密钥不进对外视图也不进审计() {
        let service = service();
        let mut changes = BTreeMap::new();
        changes.insert("llm_api_key".to_string(), json!("sk-real"));
        service.update(changes, 0, "desktop:test", at(0)).unwrap();

        // 对外视图只有占位符。
        let view = service.current();
        assert_eq!(view.values["llm_api_key"], json!(SECRET_MASK));
        // 审计里没有明文。
        let audit = service.audit(10).unwrap();
        assert!(
            audit
                .iter()
                .all(|entry| !entry.after_json.contains("sk-real")),
            "审计不该留下明文密钥"
        );
    }

    #[test]
    fn 端点与密钥标记为需重启() {
        let service = service();
        let mut changes = BTreeMap::new();
        changes.insert("llm_api_url".to_string(), json!("http://example.test/v1"));
        let updated = service.update(changes, 0, "desktop:test", at(0)).unwrap();
        assert_eq!(updated.restart_required, vec!["llm_api_url".to_string()]);
    }

    #[test]
    fn 模型名与阈值是热生效的() {
        let service = service();
        let mut changes = BTreeMap::new();
        changes.insert("llm_model".to_string(), json!("some-model"));
        changes.insert("memory_recall_min_similarity".to_string(), json!(0.5));
        let updated = service.update(changes, 0, "desktop:test", at(0)).unwrap();
        assert!(updated.restart_required.is_empty());
        assert_eq!(service.runtime().snapshot().llm.model, "some-model");
        assert_eq!(
            service.runtime().snapshot().memory.recall_min_similarity,
            0.5
        );
    }

    #[test]
    fn 接入键不从_env播种() {
        let service = service();
        service.seed_from_bootstrap(at(0)).unwrap();
        let stored = service.store.load_values().unwrap();
        assert!(
            !stored.contains_key("llm_api_key"),
            "密钥不该被播种进库——它留在 .env 里直到用户显式改"
        );
        assert!(!stored.contains_key("llm_api_url"), "端点不该被播种进库");
        assert!(
            stored.contains_key("intent_enabled"),
            "策略键仍应从 .env 播种"
        );
    }

    #[test]
    fn 库里不认识的键被忽略而不阻塞启动() {
        let service = service();
        // 模拟库里留着一个当前版本已删除的键。
        let mut stale = BTreeMap::new();
        stale.insert("removed_key".to_string(), json!("x"));
        service
            .store
            .apply_changes(0, &stale, &BTreeSet::new(), "test", at(0))
            .unwrap();

        let fresh =
            SettingsService::new(service.store.db.clone()).expect("未知键不该让配置服务起不来");
        assert!(fresh.current().values.contains_key("intent_enabled"));
    }
}
