//! 运行时配置契约：键、默认值、类型、校验与热更新标记。
//!
//! Bootstrap 配置（数据库路径、密钥、监听地址、embedding 维度）仍留在 `.env`；
//! 本文件只覆盖打开数据库后可以安全变更的运行策略。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;
use crate::error::{Result, SqlError};

/// 可持久化、可热更新的运行时配置键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingsKey {
    IntentEnabled,
    IntentMinMinutes,
    IntentMaxMinutes,
    IntentQuietHours,
    IntentMinSilenceHours,
    IntentMaxIdleDays,
    MemoryRecallMinSimilarity,
    MemoryDedupMinSimilarity,
    IdentityEvolutionEnabled,
    IdentityEvolutionIntervalHours,
}

impl SettingsKey {
    pub const ALL: [Self; 10] = [
        Self::IntentEnabled,
        Self::IntentMinMinutes,
        Self::IntentMaxMinutes,
        Self::IntentQuietHours,
        Self::IntentMinSilenceHours,
        Self::IntentMaxIdleDays,
        Self::MemoryRecallMinSimilarity,
        Self::MemoryDedupMinSimilarity,
        Self::IdentityEvolutionEnabled,
        Self::IdentityEvolutionIntervalHours,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntentEnabled => "intent_enabled",
            Self::IntentMinMinutes => "intent_min_minutes",
            Self::IntentMaxMinutes => "intent_max_minutes",
            Self::IntentQuietHours => "intent_quiet_hours",
            Self::IntentMinSilenceHours => "intent_min_silence_hours",
            Self::IntentMaxIdleDays => "intent_max_idle_days",
            Self::MemoryRecallMinSimilarity => "memory_recall_min_similarity",
            Self::MemoryDedupMinSimilarity => "memory_dedup_min_similarity",
            Self::IdentityEvolutionEnabled => "identity_evolution_enabled",
            Self::IdentityEvolutionIntervalHours => "identity_evolution_interval_hours",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let key = raw.trim();
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == key)
            .ok_or_else(|| SqlError::NotFound(format!("未知配置键：{raw}")))
    }

    pub fn kind(self) -> &'static str {
        match self {
            Self::IntentEnabled | Self::IdentityEvolutionEnabled => "boolean",
            Self::MemoryRecallMinSimilarity | Self::MemoryDedupMinSimilarity => "number",
            Self::IntentQuietHours => "string",
            _ => "integer",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::IntentEnabled => "主动开口总开关",
            Self::IntentMinMinutes => "主动开口随机间隔下限（分钟）",
            Self::IntentMaxMinutes => "主动开口随机间隔上限（分钟）",
            Self::IntentQuietHours => "安静时段，格式 HH-HH",
            Self::IntentMinSilenceHours => "用户最后发言后至少静默多少小时",
            Self::IntentMaxIdleDays => "用户沉默超过多少天后停止主动联系",
            Self::MemoryRecallMinSimilarity => "记忆召回最低余弦相似度",
            Self::MemoryDedupMinSimilarity => "记忆去重最低余弦相似度",
            Self::IdentityEvolutionEnabled => "身份演化后台任务总开关",
            Self::IdentityEvolutionIntervalHours => "身份演化检查周期（小时）",
        }
    }
}

/// 当前运行时配置快照。字段与旧 `Config` 同名，便于逐点替换读取。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSettings {
    pub intent_enabled: bool,
    pub intent_min_minutes: i64,
    pub intent_max_minutes: i64,
    pub intent_quiet_hours: (u32, u32),
    pub intent_min_silence_hours: i64,
    pub intent_max_idle_days: i64,
    pub memory_recall_min_similarity: f32,
    pub memory_dedup_min_similarity: f32,
    pub identity_evolution_enabled: bool,
    pub identity_evolution_interval_hours: i64,
}

impl RuntimeSettings {
    /// `.env` / 代码默认值；只在数据库尚无对应键时兜底。
    pub fn from_bootstrap(cfg: &Config) -> Self {
        Self {
            intent_enabled: cfg.intent_enabled,
            intent_min_minutes: cfg.intent_min_minutes,
            intent_max_minutes: cfg.intent_max_minutes,
            intent_quiet_hours: cfg.intent_quiet_hours,
            intent_min_silence_hours: cfg.intent_min_silence_hours,
            intent_max_idle_days: cfg.intent_max_idle_days,
            memory_recall_min_similarity: cfg.memory_recall_min_similarity,
            memory_dedup_min_similarity: cfg.memory_dedup_min_similarity,
            identity_evolution_enabled: cfg.identity_evolution_enabled,
            identity_evolution_interval_hours: cfg.identity_evolution_interval_hours,
        }
    }

    pub fn to_map(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                SettingsKey::IntentEnabled.as_str().to_string(),
                json!(self.intent_enabled),
            ),
            (
                SettingsKey::IntentMinMinutes.as_str().to_string(),
                json!(self.intent_min_minutes),
            ),
            (
                SettingsKey::IntentMaxMinutes.as_str().to_string(),
                json!(self.intent_max_minutes),
            ),
            (
                SettingsKey::IntentQuietHours.as_str().to_string(),
                json!(format!(
                    "{}-{}",
                    self.intent_quiet_hours.0, self.intent_quiet_hours.1
                )),
            ),
            (
                SettingsKey::IntentMinSilenceHours.as_str().to_string(),
                json!(self.intent_min_silence_hours),
            ),
            (
                SettingsKey::IntentMaxIdleDays.as_str().to_string(),
                json!(self.intent_max_idle_days),
            ),
            (
                SettingsKey::MemoryRecallMinSimilarity.as_str().to_string(),
                json!(self.memory_recall_min_similarity),
            ),
            (
                SettingsKey::MemoryDedupMinSimilarity.as_str().to_string(),
                json!(self.memory_dedup_min_similarity),
            ),
            (
                SettingsKey::IdentityEvolutionEnabled.as_str().to_string(),
                json!(self.identity_evolution_enabled),
            ),
            (
                SettingsKey::IdentityEvolutionIntervalHours
                    .as_str()
                    .to_string(),
                json!(self.identity_evolution_interval_hours),
            ),
        ])
    }

    /// 在现有快照上叠加一批原始 JSON 值，再做跨字段校验。
    pub fn apply(
        &self,
        changes: &BTreeMap<String, Value>,
    ) -> Result<(Self, BTreeMap<String, Value>)> {
        let mut next = self.clone();
        let mut normalized = BTreeMap::new();
        for (raw_key, value) in changes {
            let key = SettingsKey::parse(raw_key)?;
            match key {
                SettingsKey::IntentEnabled => {
                    next.intent_enabled = bool_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.intent_enabled));
                }
                SettingsKey::IntentMinMinutes => {
                    next.intent_min_minutes = int_value(key, value, 1)?;
                    normalized.insert(raw_key.clone(), json!(next.intent_min_minutes));
                }
                SettingsKey::IntentMaxMinutes => {
                    next.intent_max_minutes = int_value(key, value, 1)?;
                    normalized.insert(raw_key.clone(), json!(next.intent_max_minutes));
                }
                SettingsKey::IntentQuietHours => {
                    next.intent_quiet_hours = quiet_hours_value(key, value)?;
                    normalized.insert(
                        raw_key.clone(),
                        json!(format!(
                            "{}-{}",
                            next.intent_quiet_hours.0, next.intent_quiet_hours.1
                        )),
                    );
                }
                SettingsKey::IntentMinSilenceHours => {
                    next.intent_min_silence_hours = int_value(key, value, 0)?;
                    normalized.insert(raw_key.clone(), json!(next.intent_min_silence_hours));
                }
                SettingsKey::IntentMaxIdleDays => {
                    next.intent_max_idle_days = int_value(key, value, 0)?;
                    normalized.insert(raw_key.clone(), json!(next.intent_max_idle_days));
                }
                SettingsKey::MemoryRecallMinSimilarity => {
                    next.memory_recall_min_similarity = similarity_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.memory_recall_min_similarity));
                }
                SettingsKey::MemoryDedupMinSimilarity => {
                    next.memory_dedup_min_similarity = similarity_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.memory_dedup_min_similarity));
                }
                SettingsKey::IdentityEvolutionEnabled => {
                    next.identity_evolution_enabled = bool_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.identity_evolution_enabled));
                }
                SettingsKey::IdentityEvolutionIntervalHours => {
                    next.identity_evolution_interval_hours = int_value(key, value, 1)?;
                    normalized.insert(
                        raw_key.clone(),
                        json!(next.identity_evolution_interval_hours),
                    );
                }
            }
        }
        next.validate()?;
        Ok((next, normalized))
    }

    pub fn validate(&self) -> Result<()> {
        if self.intent_min_minutes > self.intent_max_minutes {
            return Err(SqlError::Config(
                "intent_min_minutes 不能大于 intent_max_minutes".to_string(),
            ));
        }
        if self.intent_quiet_hours.0 > 23 || self.intent_quiet_hours.1 > 23 {
            return Err(SqlError::Config(
                "intent_quiet_hours 必须是 0-23 的小时".to_string(),
            ));
        }
        if self.memory_dedup_min_similarity < self.memory_recall_min_similarity {
            return Err(SqlError::Config(
                "memory_dedup_min_similarity 必须不小于 memory_recall_min_similarity".to_string(),
            ));
        }
        Ok(())
    }
}

/// 配置 schema 项，供桌面 / Linux / Android 生成表单。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingsSchemaItem {
    pub key: String,
    pub kind: String,
    pub description: String,
    pub default: Value,
    pub current: Value,
    pub hot_reload: bool,
    pub restart_required: bool,
}

pub fn schema(current: &RuntimeSettings) -> Vec<SettingsSchemaItem> {
    let defaults = RuntimeSettings::from_bootstrap(Config::get()).to_map();
    let current = current.to_map();
    SettingsKey::ALL
        .into_iter()
        .map(|key| SettingsSchemaItem {
            key: key.as_str().to_string(),
            kind: key.kind().to_string(),
            description: key.description().to_string(),
            default: defaults.get(key.as_str()).cloned().unwrap_or(Value::Null),
            current: current.get(key.as_str()).cloned().unwrap_or(Value::Null),
            hot_reload: true,
            restart_required: false,
        })
        .collect()
}

fn bool_value(key: SettingsKey, value: &Value) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| SqlError::Config(format!("{} 必须是布尔值", key.as_str())))
}

fn int_value(key: SettingsKey, value: &Value, min: i64) -> Result<i64> {
    let parsed = value
        .as_i64()
        .ok_or_else(|| SqlError::Config(format!("{} 必须是整数", key.as_str())))?;
    if parsed < min {
        return Err(SqlError::Config(format!("{} 不能小于 {min}", key.as_str())));
    }
    Ok(parsed)
}

fn similarity_value(key: SettingsKey, value: &Value) -> Result<f32> {
    let parsed = value
        .as_f64()
        .ok_or_else(|| SqlError::Config(format!("{} 必须是 0~1 的数字", key.as_str())))?;
    if !(0.0..=1.0).contains(&parsed) {
        return Err(SqlError::Config(format!("{} 必须位于 0~1", key.as_str())));
    }
    Ok(parsed as f32)
}

fn quiet_hours_value(key: SettingsKey, value: &Value) -> Result<(u32, u32)> {
    let raw = value
        .as_str()
        .ok_or_else(|| SqlError::Config(format!("{} 必须是 HH-HH 字符串", key.as_str())))?;
    let mut parts = raw.split('-').map(str::trim);
    let start = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v <= 23)
        .ok_or_else(|| SqlError::Config(format!("{} 起始小时无效", key.as_str())))?;
    let end = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v <= 23)
        .ok_or_else(|| SqlError::Config(format!("{} 结束小时无效", key.as_str())))?;
    if parts.next().is_some() {
        return Err(SqlError::Config(format!(
            "{} 必须是 HH-HH 字符串",
            key.as_str()
        )));
    }
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_values_and_cross_field_breaks() {
        let base = RuntimeSettings::from_bootstrap(Config::get());
        let mut changes = BTreeMap::new();
        changes.insert("intent_min_minutes".to_string(), json!(100));
        changes.insert("intent_max_minutes".to_string(), json!(10));
        assert!(base.apply(&changes).is_err(), "最小值超过当前最大值应拒绝");

        let mut changes = BTreeMap::new();
        changes.insert("memory_recall_min_similarity".to_string(), json!(0.98));
        changes.insert("memory_dedup_min_similarity".to_string(), json!(0.90));
        assert!(base.apply(&changes).is_err(), "去重阈值不能低于召回阈值");

        let mut changes = BTreeMap::new();
        changes.insert("intent_quiet_hours".to_string(), json!("25-7"));
        assert!(base.apply(&changes).is_err());
    }

    #[test]
    fn normalizes_values_and_returns_typed_snapshot() {
        let base = RuntimeSettings::from_bootstrap(Config::get());
        let mut changes = BTreeMap::new();
        changes.insert("intent_min_minutes".to_string(), json!(10));
        changes.insert("intent_max_minutes".to_string(), json!(20));
        changes.insert("intent_quiet_hours".to_string(), json!(" 23 - 7 "));
        let (next, normalized) = base.apply(&changes).unwrap();
        assert_eq!(next.intent_min_minutes, 10);
        assert_eq!(next.intent_quiet_hours, (23, 7));
        assert_eq!(normalized["intent_quiet_hours"], json!("23-7"));
    }

    #[test]
    fn schema_covers_every_persisted_key() {
        let current = RuntimeSettings::from_bootstrap(Config::get());
        let items = schema(&current);
        assert_eq!(items.len(), SettingsKey::ALL.len());
        assert!(items.iter().all(|item| item.hot_reload));
        assert!(items.iter().all(|item| !item.restart_required));
    }
}
