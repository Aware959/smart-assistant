//! 运行时配置契约：分组、键、默认值、类型、校验与重载语义。
//!
//! 配置分两层：
//!
//! - **启动期固定**（`.env`）：路径、监听地址、日志文件、向量签名等。改它们要改文件、
//!   要重启，部分还牵连数据库结构（向量维度 / 模型）。这一层不属于本文件，
//!   只在 [`SettingsGroup::env_vars`] 里登记归属——面板不提供编辑。
//! - **运行期可改**（数据库）：本文件的 [`SettingsKey`] 就是这一层的全部键。
//!
//! 生效顺序：**环境变量是缺省值，数据库里显式改过的值覆盖它**。
//! 面板改一个键 = 写一行数据库；`.env` 不动，它仍然决定"没被覆盖时"的值。
//! 面板同时给出缺省值与当前值，两者不同即说明该项已被面板覆盖。
//!
//! 分组不是装饰：同组的键共享一套校验与重载语义，缺省值同源，
//! 面板按组渲染成一张卡片。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{Config, LlmProvider};
use crate::error::{Result, SqlError};

/// 密钥类取值在视图与审计里的占位符；客户端原样回传即表示"不改这一项"。
///
/// 密钥**不回落**到明文：视图只给占位符，审计只记"变了"不记值。
pub const SECRET_MASK: &str = "********";

/// 配置分组。
///
/// 顺序即面板里的呈现顺序（从"最常改"到"基本不动"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingsGroup {
    /// 大模型：端点、密钥、模型、结构化输出模式。
    Llm,
    /// 向量：嵌入端点与密钥（维度 / 模型属启动期固定，见 [`Self::env_vars`]）。
    Embedding,
    /// 记忆：召回与去重阈值。
    Memory,
    /// 主动开口：开关、间隔、安静时段。
    Intent,
    /// 身份演化：开关与周期。
    Identity,
    /// 通道：Telegram / 微信 iLink（启动期固定）。
    Channel,
    /// 工具：MCP 客户端与贴纸目录（启动期固定）。
    Tool,
    /// 程序本身：路径、监听、日志、数据库（启动期固定）。
    Program,
}

impl SettingsGroup {
    pub const ALL: [Self; 8] = [
        Self::Llm,
        Self::Embedding,
        Self::Memory,
        Self::Intent,
        Self::Identity,
        Self::Channel,
        Self::Tool,
        Self::Program,
    ];

    /// 分组标识（面板与接口里用的稳定字符串）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Embedding => "embedding",
            Self::Memory => "memory",
            Self::Intent => "intent",
            Self::Identity => "identity",
            Self::Channel => "channel",
            Self::Tool => "tool",
            Self::Program => "program",
        }
    }

    /// 分组标题（面板上的卡片标题）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Llm => "大模型",
            Self::Embedding => "向量",
            Self::Memory => "记忆",
            Self::Intent => "主动开口",
            Self::Identity => "身份演化",
            Self::Channel => "通道",
            Self::Tool => "工具",
            Self::Program => "程序本身",
        }
    }

    /// 本组涉及的全部环境变量——含启动期固定项，用于 `.env` 与分组对照。
    pub fn env_vars(self) -> &'static [&'static str] {
        match self {
            Self::Llm => &[
                "LLM_PROVIDER",
                "LLM_API_URL",
                "LLM_API_KEY",
                "LLM_MODEL",
                "LLM_STRUCTURED_OUTPUT",
            ],
            Self::Embedding => &[
                "EMBEDDING_API_URL",
                "EMBEDDING_API_KEY",
                "EMBEDDING_MODEL",
                "EMBEDDING_DIM",
            ],
            Self::Memory => &[
                "MEMORY_RECALL_MIN_SIMILARITY",
                "MEMORY_DEDUP_MIN_SIMILARITY",
            ],
            Self::Intent => &[
                "INTENT_ENABLED",
                "INTENT_MIN_MINUTES",
                "INTENT_MAX_MINUTES",
                "INTENT_QUIET_HOURS",
                "INTENT_MIN_SILENCE_HOURS",
                "INTENT_MAX_IDLE_DAYS",
            ],
            Self::Identity => &[
                "IDENTITY_EVOLUTION_ENABLED",
                "IDENTITY_EVOLUTION_INTERVAL_HOURS",
            ],
            Self::Channel => &[
                "TELEGRAM_ENABLED",
                "TELEGRAM_BOT_TOKEN",
                "TELEGRAM_ALLOWED_IDS",
                "TELEGRAM_PROXY",
                "ILINK_ENABLED",
                "ILINK_BOT_TOKEN",
                "ILINK_SESSION_FILE",
                "ILINK_ALLOWED_IDS",
                "ILINK_CONTEXT_MAX_AGE_HOURS",
            ],
            Self::Tool => &[
                "MCP_ENABLED",
                "MCP_CONFIG_FILE",
                "MCP_CONFIRM_DEFAULT",
                "MCP_CONFIRM_TIMEOUT_SECS",
                "MCP_RESULT_MAX_CHARS",
                "STICKER_CATALOG_FILE",
            ],
            Self::Program => &[
                "ZHIWEI_ENV",
                "SMART_ASSISTANT_DB",
                "RUST_LOG",
                "SMART_ASSISTANT_LOG_FILE",
                "SMART_ASSISTANT_TRACE_FILE",
                "TRACE_CONTENT",
                "SMART_ASSISTANT_WATCHDOG_FILE",
                "CONTROL_ENABLED",
                "CONTROL_ADDR",
                "CONTROL_STATE_FILE",
                "CONTROL_TOKEN_FILE",
                "HTTPS_PROXY",
                "ALL_PROXY",
            ],
        }
    }
}

/// 可持久化、可热更新的运行时配置键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingsKey {
    // —— 大模型组 ——
    LlmProvider,
    LlmApiUrl,
    LlmApiKey,
    LlmModel,
    LlmStructuredOutput,
    // —— 向量组 ——
    EmbeddingApiUrl,
    EmbeddingApiKey,
    // —— 记忆组 ——
    MemoryRecallMinSimilarity,
    MemoryDedupMinSimilarity,
    // —— 主动开口组 ——
    IntentEnabled,
    IntentMinMinutes,
    IntentMaxMinutes,
    IntentQuietHours,
    IntentMinSilenceHours,
    IntentMaxIdleDays,
    // —— 身份演化组 ——
    IdentityEvolutionEnabled,
    IdentityEvolutionIntervalHours,
}

impl SettingsKey {
    pub const ALL: [Self; 17] = [
        Self::LlmProvider,
        Self::LlmApiUrl,
        Self::LlmApiKey,
        Self::LlmModel,
        Self::LlmStructuredOutput,
        Self::EmbeddingApiUrl,
        Self::EmbeddingApiKey,
        Self::MemoryRecallMinSimilarity,
        Self::MemoryDedupMinSimilarity,
        Self::IntentEnabled,
        Self::IntentMinMinutes,
        Self::IntentMaxMinutes,
        Self::IntentQuietHours,
        Self::IntentMinSilenceHours,
        Self::IntentMaxIdleDays,
        Self::IdentityEvolutionEnabled,
        Self::IdentityEvolutionIntervalHours,
    ];

    /// 键名（数据库里的键、环境变量的大写形式）。
    ///
    /// 不变量：`key.to_uppercase() == 环境变量名`。面板、CLI、`.env` 三处共用一套名字，
    /// 不维护映射表。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LlmProvider => "llm_provider",
            Self::LlmApiUrl => "llm_api_url",
            Self::LlmApiKey => "llm_api_key",
            Self::LlmModel => "llm_model",
            Self::LlmStructuredOutput => "llm_structured_output",
            Self::EmbeddingApiUrl => "embedding_api_url",
            Self::EmbeddingApiKey => "embedding_api_key",
            Self::MemoryRecallMinSimilarity => "memory_recall_min_similarity",
            Self::MemoryDedupMinSimilarity => "memory_dedup_min_similarity",
            Self::IntentEnabled => "intent_enabled",
            Self::IntentMinMinutes => "intent_min_minutes",
            Self::IntentMaxMinutes => "intent_max_minutes",
            Self::IntentQuietHours => "intent_quiet_hours",
            Self::IntentMinSilenceHours => "intent_min_silence_hours",
            Self::IntentMaxIdleDays => "intent_max_idle_days",
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

    /// 该键归属的分组。
    pub fn group(self) -> SettingsGroup {
        match self {
            Self::LlmProvider
            | Self::LlmApiUrl
            | Self::LlmApiKey
            | Self::LlmModel
            | Self::LlmStructuredOutput => SettingsGroup::Llm,
            Self::EmbeddingApiUrl | Self::EmbeddingApiKey => SettingsGroup::Embedding,
            Self::MemoryRecallMinSimilarity | Self::MemoryDedupMinSimilarity => {
                SettingsGroup::Memory
            }
            Self::IntentEnabled
            | Self::IntentMinMinutes
            | Self::IntentMaxMinutes
            | Self::IntentQuietHours
            | Self::IntentMinSilenceHours
            | Self::IntentMaxIdleDays => SettingsGroup::Intent,
            Self::IdentityEvolutionEnabled | Self::IdentityEvolutionIntervalHours => {
                SettingsGroup::Identity
            }
        }
    }

    /// 取值类型。
    ///
    /// `secret` 是密钥类：视图只给占位符，保存时原样回传表示"不改"。
    pub fn kind(self) -> &'static str {
        match self {
            Self::LlmProvider
            | Self::LlmApiUrl
            | Self::LlmModel
            | Self::LlmStructuredOutput
            | Self::EmbeddingApiUrl
            | Self::IntentQuietHours => "string",
            Self::LlmApiKey | Self::EmbeddingApiKey => "secret",
            Self::IntentEnabled
            | Self::IdentityEvolutionEnabled
            | Self::MemoryRecallMinSimilarity
            | Self::MemoryDedupMinSimilarity => {
                if matches!(
                    self,
                    Self::MemoryRecallMinSimilarity | Self::MemoryDedupMinSimilarity
                ) {
                    "number"
                } else {
                    "boolean"
                }
            }
            Self::IntentMinMinutes
            | Self::IntentMaxMinutes
            | Self::IntentMinSilenceHours
            | Self::IntentMaxIdleDays
            | Self::IdentityEvolutionIntervalHours => "integer",
        }
    }

    /// 是否为密钥类取值。
    pub fn is_secret(self) -> bool {
        self.kind() == "secret"
    }

    /// 该键是否属于"运行策略"（首次启动时从 `.env` 播种进数据库）。
    ///
    /// 策略键（记忆 / 主动开口 / 身份演化）播种后由数据库说了算，`.env` 里的旧值
    /// 不再回读——这是既有行为。**接入键**（大模型 / 向量）不播种：它们是用户会
    /// 反复在 `.env` 与面板之间挪的东西，播种会让"改了 `.env` 却不生效"变成常态。
    /// 接入键只存"面板显式改过"的那一次覆盖。
    pub fn is_policy(self) -> bool {
        matches!(
            self.group(),
            SettingsGroup::Memory | SettingsGroup::Intent | SettingsGroup::Identity
        )
    }

    /// 改完是否要重启进程才生效。
    ///
    /// 判据是**值被谁读、什么时候读**：
    /// - 每轮现读的（模型名、结构化输出模式、阈值、开关、间隔）→ 热生效；
    /// - 烤进长连接客户端的（端点、密钥、提供方）→ 重启生效。
    pub fn restart_required(self) -> bool {
        matches!(
            self,
            Self::LlmProvider
                | Self::LlmApiUrl
                | Self::LlmApiKey
                | Self::EmbeddingApiUrl
                | Self::EmbeddingApiKey
        )
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::LlmProvider => "大模型提供方：openai（OpenAI 兼容）或 google（Gemini 原生）",
            Self::LlmApiUrl => "大模型端点；留空用提供方默认地址",
            Self::LlmApiKey => "大模型密钥；留空回退 OPENAI_API_KEY 环境变量",
            Self::LlmModel => "对话模型名",
            Self::LlmStructuredOutput => "结构化输出模式：json_schema 或 json_object",
            Self::EmbeddingApiUrl => "向量端点",
            Self::EmbeddingApiKey => "向量密钥；留空回退 OPENAI_API_KEY 环境变量",
            Self::MemoryRecallMinSimilarity => "记忆召回最低余弦相似度",
            Self::MemoryDedupMinSimilarity => "记忆去重最低余弦相似度",
            Self::IntentEnabled => "主动开口总开关",
            Self::IntentMinMinutes => "主动开口随机间隔下限（分钟）",
            Self::IntentMaxMinutes => "主动开口随机间隔上限（分钟）",
            Self::IntentQuietHours => "安静时段，格式 HH-HH",
            Self::IntentMinSilenceHours => "用户最后发言后至少静默多少小时",
            Self::IntentMaxIdleDays => "用户沉默超过多少天后停止主动联系",
            Self::IdentityEvolutionEnabled => "身份演化后台任务总开关",
            Self::IdentityEvolutionIntervalHours => "身份演化检查周期（小时）",
        }
    }
}

/// 大模型组的当前值。
///
/// 密钥不进 `Debug` 输出——这个结构会被日志与错误信息带出去。
#[derive(Clone, PartialEq)]
pub struct LlmSettings {
    pub provider: LlmProvider,
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub structured_output: String,
}

impl std::fmt::Debug for LlmSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmSettings")
            .field("provider", &self.provider)
            .field("api_url", &self.api_url)
            .field("api_key", &SECRET_MASK)
            .field("model", &self.model)
            .field("structured_output", &self.structured_output)
            .finish()
    }
}

/// 向量组的当前值。
#[derive(Clone, PartialEq)]
pub struct EmbeddingSettings {
    pub api_url: String,
    pub api_key: String,
}

impl std::fmt::Debug for EmbeddingSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingSettings")
            .field("api_url", &self.api_url)
            .field("api_key", &SECRET_MASK)
            .finish()
    }
}

/// 记忆组的当前值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemorySettings {
    pub recall_min_similarity: f32,
    pub dedup_min_similarity: f32,
}

/// 主动开口组的当前值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntentSettings {
    pub enabled: bool,
    pub min_minutes: i64,
    pub max_minutes: i64,
    pub quiet_hours: (u32, u32),
    pub min_silence_hours: i64,
    pub max_idle_days: i64,
}

/// 身份演化组的当前值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdentitySettings {
    pub evolution_enabled: bool,
    pub evolution_interval_hours: i64,
}

/// 当前运行时配置快照：按组装配。
///
/// 字段与旧 `Config` 同名，便于逐点替换读取。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSettings {
    pub llm: LlmSettings,
    pub embedding: EmbeddingSettings,
    pub memory: MemorySettings,
    pub intent: IntentSettings,
    pub identity: IdentitySettings,
}

impl RuntimeSettings {
    /// `.env` / 代码默认值；只在数据库尚无对应键时兜底。
    pub fn from_bootstrap(cfg: &Config) -> Self {
        Self {
            llm: LlmSettings {
                provider: cfg.llm_provider,
                api_url: cfg.llm_api_url.clone(),
                api_key: cfg.llm_api_key.clone(),
                model: cfg.llm_model.clone(),
                structured_output: normalize_structured_output(&cfg.llm_structured_output),
            },
            embedding: EmbeddingSettings {
                api_url: cfg.embedding_api_url.clone(),
                api_key: cfg.embedding_api_key.clone(),
            },
            memory: MemorySettings {
                recall_min_similarity: cfg.memory_recall_min_similarity,
                dedup_min_similarity: cfg.memory_dedup_min_similarity,
            },
            intent: IntentSettings {
                enabled: cfg.intent_enabled,
                min_minutes: cfg.intent_min_minutes,
                max_minutes: cfg.intent_max_minutes,
                quiet_hours: cfg.intent_quiet_hours,
                min_silence_hours: cfg.intent_min_silence_hours,
                max_idle_days: cfg.intent_max_idle_days,
            },
            identity: IdentitySettings {
                evolution_enabled: cfg.identity_evolution_enabled,
                evolution_interval_hours: cfg.identity_evolution_interval_hours,
            },
        }
    }

    /// 展平成键 → 值（写库、比对缺省值都用这个）。
    pub fn to_map(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                SettingsKey::LlmProvider.as_str().to_string(),
                json!(provider_name(self.llm.provider)),
            ),
            (
                SettingsKey::LlmApiUrl.as_str().to_string(),
                json!(self.llm.api_url),
            ),
            (
                SettingsKey::LlmApiKey.as_str().to_string(),
                json!(self.llm.api_key),
            ),
            (
                SettingsKey::LlmModel.as_str().to_string(),
                json!(self.llm.model),
            ),
            (
                SettingsKey::LlmStructuredOutput.as_str().to_string(),
                json!(self.llm.structured_output),
            ),
            (
                SettingsKey::EmbeddingApiUrl.as_str().to_string(),
                json!(self.embedding.api_url),
            ),
            (
                SettingsKey::EmbeddingApiKey.as_str().to_string(),
                json!(self.embedding.api_key),
            ),
            (
                SettingsKey::MemoryRecallMinSimilarity.as_str().to_string(),
                json!(self.memory.recall_min_similarity),
            ),
            (
                SettingsKey::MemoryDedupMinSimilarity.as_str().to_string(),
                json!(self.memory.dedup_min_similarity),
            ),
            (
                SettingsKey::IntentEnabled.as_str().to_string(),
                json!(self.intent.enabled),
            ),
            (
                SettingsKey::IntentMinMinutes.as_str().to_string(),
                json!(self.intent.min_minutes),
            ),
            (
                SettingsKey::IntentMaxMinutes.as_str().to_string(),
                json!(self.intent.max_minutes),
            ),
            (
                SettingsKey::IntentQuietHours.as_str().to_string(),
                json!(format!(
                    "{}-{}",
                    self.intent.quiet_hours.0, self.intent.quiet_hours.1
                )),
            ),
            (
                SettingsKey::IntentMinSilenceHours.as_str().to_string(),
                json!(self.intent.min_silence_hours),
            ),
            (
                SettingsKey::IntentMaxIdleDays.as_str().to_string(),
                json!(self.intent.max_idle_days),
            ),
            (
                SettingsKey::IdentityEvolutionEnabled.as_str().to_string(),
                json!(self.identity.evolution_enabled),
            ),
            (
                SettingsKey::IdentityEvolutionIntervalHours
                    .as_str()
                    .to_string(),
                json!(self.identity.evolution_interval_hours),
            ),
        ])
    }

    /// 对外的视图：密钥类只给占位符。
    ///
    /// 面板每次保存都会把看到的占位符原样回传，服务端据此判定"这一项没改"
    /// （见 [`crate::core::settings::SettingsService::update`]）。
    /// **未配置**的密钥给空串而不是占位符——"没配"和"配了但不给看"是两件事。
    pub fn to_public_map(&self) -> BTreeMap<String, Value> {
        let mut map = self.to_map();
        for key in SettingsKey::ALL {
            if key.is_secret() {
                let configured = map
                    .get(key.as_str())
                    .and_then(|value| value.as_str())
                    .map(|text| !text.trim().is_empty())
                    .unwrap_or(false);
                map.insert(
                    key.as_str().to_string(),
                    if configured {
                        json!(SECRET_MASK)
                    } else {
                        json!("")
                    },
                );
            }
        }
        map
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
                SettingsKey::LlmProvider => {
                    next.llm.provider = parse_provider(key, value)?;
                    normalized.insert(raw_key.clone(), json!(provider_name(next.llm.provider)));
                }
                SettingsKey::LlmApiUrl => {
                    next.llm.api_url = string_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.llm.api_url));
                }
                SettingsKey::LlmApiKey => {
                    next.llm.api_key = string_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.llm.api_key));
                }
                SettingsKey::LlmModel => {
                    next.llm.model = string_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.llm.model));
                }
                SettingsKey::LlmStructuredOutput => {
                    next.llm.structured_output = structured_output_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.llm.structured_output));
                }
                SettingsKey::EmbeddingApiUrl => {
                    next.embedding.api_url = string_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.embedding.api_url));
                }
                SettingsKey::EmbeddingApiKey => {
                    next.embedding.api_key = string_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.embedding.api_key));
                }
                SettingsKey::MemoryRecallMinSimilarity => {
                    next.memory.recall_min_similarity = similarity_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.memory.recall_min_similarity));
                }
                SettingsKey::MemoryDedupMinSimilarity => {
                    next.memory.dedup_min_similarity = similarity_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.memory.dedup_min_similarity));
                }
                SettingsKey::IntentEnabled => {
                    next.intent.enabled = bool_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.intent.enabled));
                }
                SettingsKey::IntentMinMinutes => {
                    next.intent.min_minutes = int_value(key, value, 1)?;
                    normalized.insert(raw_key.clone(), json!(next.intent.min_minutes));
                }
                SettingsKey::IntentMaxMinutes => {
                    next.intent.max_minutes = int_value(key, value, 1)?;
                    normalized.insert(raw_key.clone(), json!(next.intent.max_minutes));
                }
                SettingsKey::IntentQuietHours => {
                    next.intent.quiet_hours = quiet_hours_value(key, value)?;
                    normalized.insert(
                        raw_key.clone(),
                        json!(format!(
                            "{}-{}",
                            next.intent.quiet_hours.0, next.intent.quiet_hours.1
                        )),
                    );
                }
                SettingsKey::IntentMinSilenceHours => {
                    next.intent.min_silence_hours = int_value(key, value, 0)?;
                    normalized.insert(raw_key.clone(), json!(next.intent.min_silence_hours));
                }
                SettingsKey::IntentMaxIdleDays => {
                    next.intent.max_idle_days = int_value(key, value, 0)?;
                    normalized.insert(raw_key.clone(), json!(next.intent.max_idle_days));
                }
                SettingsKey::IdentityEvolutionEnabled => {
                    next.identity.evolution_enabled = bool_value(key, value)?;
                    normalized.insert(raw_key.clone(), json!(next.identity.evolution_enabled));
                }
                SettingsKey::IdentityEvolutionIntervalHours => {
                    next.identity.evolution_interval_hours = int_value(key, value, 1)?;
                    normalized.insert(
                        raw_key.clone(),
                        json!(next.identity.evolution_interval_hours),
                    );
                }
            }
        }
        next.validate()?;
        Ok((next, normalized))
    }

    pub fn validate(&self) -> Result<()> {
        if self.intent.min_minutes > self.intent.max_minutes {
            return Err(SqlError::Config(
                "intent_min_minutes 不能大于 intent_max_minutes".to_string(),
            ));
        }
        if self.intent.quiet_hours.0 > 23 || self.intent.quiet_hours.1 > 23 {
            return Err(SqlError::Config(
                "intent_quiet_hours 必须是 0-23 的小时".to_string(),
            ));
        }
        if self.memory.dedup_min_similarity < self.memory.recall_min_similarity {
            return Err(SqlError::Config(
                "memory_dedup_min_similarity 必须不小于 memory_recall_min_similarity".to_string(),
            ));
        }
        if !matches!(
            self.llm.structured_output.as_str(),
            "json_schema" | "json_object"
        ) {
            return Err(SqlError::Config(
                "llm_structured_output 必须是 json_schema 或 json_object".to_string(),
            ));
        }
        Ok(())
    }
}

/// 配置 schema 项，供桌面 / Linux / Android 生成表单。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingsSchemaItem {
    pub key: String,
    /// 所属分组（`SettingsGroup::as_str`）。
    pub group: String,
    /// 分组标题（`SettingsGroup::label`）。
    pub group_label: String,
    pub kind: String,
    pub description: String,
    pub default: Value,
    pub current: Value,
    pub hot_reload: bool,
    pub restart_required: bool,
    /// 是否可在面板里改。启动期固定的项为 `false`——只展示，不给输入框。
    pub editable: bool,
}

/// 生成 schema：可改键 + 只读键，按分组顺序排列。
///
/// `default` 是 `.env` 的缺省值，`current` 是生效值——两者不同即说明该键被面板
/// 覆盖过。密钥类的 `current` 只给占位符。
pub fn schema(current: &RuntimeSettings) -> Vec<SettingsSchemaItem> {
    let defaults = RuntimeSettings::from_bootstrap(Config::get()).to_map();
    let current = current.to_map();
    let mut items = Vec::with_capacity(SettingsKey::ALL.len() + READ_ONLY.len());
    for group in SettingsGroup::ALL {
        for key in SettingsKey::ALL {
            if key.group() != group {
                continue;
            }
            let name = key.as_str();
            items.push(SettingsSchemaItem {
                key: name.to_string(),
                group: group.as_str().to_string(),
                group_label: group.label().to_string(),
                kind: key.kind().to_string(),
                description: key.description().to_string(),
                default: masked(key, defaults.get(name)),
                current: masked(key, current.get(name)),
                hot_reload: !key.restart_required(),
                restart_required: key.restart_required(),
                editable: true,
            });
        }
        for item in READ_ONLY {
            if item.group != group {
                continue;
            }
            let value = read_env_value(item);
            items.push(SettingsSchemaItem {
                key: item.key.to_string(),
                group: group.as_str().to_string(),
                group_label: group.label().to_string(),
                kind: item.kind.to_string(),
                description: item.description.to_string(),
                default: value.clone(),
                current: value,
                hot_reload: false,
                restart_required: true,
                editable: false,
            });
        }
    }
    items
}

/// 只读项：由 `.env` 决定，改它要改文件并重启。
///
/// 键名同样满足 `key.to_uppercase() == 环境变量名`，值直接读进程环境——
/// 启动脚手架已把 `.env.{档位}` 灌进进程环境，所以读到的就是生效值。
const READ_ONLY: &[ReadOnlyItem] = &[
    // —— 通道组 ——
    ReadOnlyItem {
        key: "telegram_enabled",
        group: SettingsGroup::Channel,
        kind: "boolean",
        description: "Telegram 通道硬开关：0 = 明确不启动",
    },
    ReadOnlyItem {
        key: "telegram_bot_token",
        group: SettingsGroup::Channel,
        kind: "secret",
        description: "Telegram Bot Token（@BotFather 申请）",
    },
    ReadOnlyItem {
        key: "telegram_allowed_ids",
        group: SettingsGroup::Channel,
        kind: "string",
        description: "允许接入的 chat id 白名单，逗号分隔；留空不限制",
    },
    ReadOnlyItem {
        key: "telegram_proxy",
        group: SettingsGroup::Channel,
        kind: "string",
        description: "Telegram API 代理；留空依次尝试 HTTPS_PROXY / ALL_PROXY",
    },
    ReadOnlyItem {
        key: "ilink_enabled",
        group: SettingsGroup::Channel,
        kind: "boolean",
        description: "微信 iLink 通道开关",
    },
    ReadOnlyItem {
        key: "ilink_bot_token",
        group: SettingsGroup::Channel,
        kind: "secret",
        description: "微信 iLink bot_token（扫码后自动写入会话文件）",
    },
    ReadOnlyItem {
        key: "ilink_session_file",
        group: SettingsGroup::Channel,
        kind: "string",
        description: "微信 iLink 会话文件（保存扫码凭证）",
    },
    ReadOnlyItem {
        key: "ilink_allowed_ids",
        group: SettingsGroup::Channel,
        kind: "string",
        description: "允许接入的微信用户 id 白名单，逗号分隔",
    },
    ReadOnlyItem {
        key: "ilink_context_max_age_hours",
        group: SettingsGroup::Channel,
        kind: "integer",
        description: "iLink context_token 新鲜窗口（小时）",
    },
    // —— 向量组：签名参数（模型 / 维度）由向量表锁定，不进设置面 ——
    ReadOnlyItem {
        key: "embedding_model",
        group: SettingsGroup::Embedding,
        kind: "string",
        description: "向量模型；变更需重建向量表（zhiwei-cli db rebuild）",
    },
    ReadOnlyItem {
        key: "embedding_dim",
        group: SettingsGroup::Embedding,
        kind: "integer",
        description: "向量维度；变更需重建向量表（zhiwei-cli db rebuild）",
    },
    // —— 工具组 ——
    ReadOnlyItem {
        key: "mcp_enabled",
        group: SettingsGroup::Tool,
        kind: "boolean",
        description: "MCP 客户端总开关：接入即等于授权子进程以本机身份做事",
    },
    ReadOnlyItem {
        key: "mcp_config_file",
        group: SettingsGroup::Tool,
        kind: "string",
        description: "MCP 配置文件路径（兼容 Claude Code 的 mcpServers 格式）",
    },
    ReadOnlyItem {
        key: "mcp_confirm_default",
        group: SettingsGroup::Tool,
        kind: "boolean",
        description: "MCP 工具全局人工确认缺省：凡 MCP 必问",
    },
    ReadOnlyItem {
        key: "mcp_confirm_timeout_secs",
        group: SettingsGroup::Tool,
        kind: "integer",
        description: "等待用户确认的超时（秒）；超时视为拒绝",
    },
    ReadOnlyItem {
        key: "mcp_result_max_chars",
        group: SettingsGroup::Tool,
        kind: "integer",
        description: "MCP 工具结果回填给模型的文本上限（字符）",
    },
    ReadOnlyItem {
        key: "sticker_catalog_file",
        group: SettingsGroup::Tool,
        kind: "string",
        description: "贴纸目录 JSON 文件；文件不存在即没有贴纸能力",
    },
    // —— 程序本身组 ——
    ReadOnlyItem {
        key: "zhiwei_env",
        group: SettingsGroup::Program,
        kind: "string",
        description: "环境档位：dev / prod，决定读哪份 .env",
    },
    ReadOnlyItem {
        key: "smart_assistant_db",
        group: SettingsGroup::Program,
        kind: "string",
        description: "数据库路径",
    },
    ReadOnlyItem {
        key: "rust_log",
        group: SettingsGroup::Program,
        kind: "string",
        description: "日志级别：error / warn / info / debug / trace",
    },
    ReadOnlyItem {
        key: "smart_assistant_log_file",
        group: SettingsGroup::Program,
        kind: "string",
        description: "文本日志镜像路径；留空只输出 stdout",
    },
    ReadOnlyItem {
        key: "smart_assistant_trace_file",
        group: SettingsGroup::Program,
        kind: "string",
        description: "链路追踪 JSON 落盘路径；留空不写",
    },
    ReadOnlyItem {
        key: "trace_content",
        group: SettingsGroup::Program,
        kind: "boolean",
        description: "追踪日志是否记录内容全文",
    },
    ReadOnlyItem {
        key: "smart_assistant_watchdog_file",
        group: SettingsGroup::Program,
        kind: "string",
        description: "进程假死诊断心跳文件路径",
    },
    ReadOnlyItem {
        key: "control_enabled",
        group: SettingsGroup::Program,
        kind: "boolean",
        description: "本地控制接口开关；缺省开启，只监听 loopback",
    },
    ReadOnlyItem {
        key: "control_addr",
        group: SettingsGroup::Program,
        kind: "string",
        description: "本地控制接口监听地址；端口 0 时由系统分配",
    },
    ReadOnlyItem {
        key: "control_state_file",
        group: SettingsGroup::Program,
        kind: "string",
        description: "控制接口状态文件（PID / 端口 / 版本）",
    },
    ReadOnlyItem {
        key: "control_token_file",
        group: SettingsGroup::Program,
        kind: "string",
        description: "控制接口 token 文件；客户端读取后以 Bearer 发送",
    },
    ReadOnlyItem {
        key: "https_proxy",
        group: SettingsGroup::Program,
        kind: "string",
        description: "HTTPS 代理（通用兜底，通道与 HTTP 客户端都读）",
    },
    ReadOnlyItem {
        key: "all_proxy",
        group: SettingsGroup::Program,
        kind: "string",
        description: "全协议代理（通用兜底）",
    },
];

/// 只读项的静态描述。
struct ReadOnlyItem {
    key: &'static str,
    group: SettingsGroup,
    kind: &'static str,
    description: &'static str,
}

/// 读进程环境里的实际值（启动脚手架已把 `.env` 灌进来）。
fn read_env_value(item: &ReadOnlyItem) -> Value {
    let raw = match std::env::var(item.key.to_uppercase()) {
        Ok(value) => value,
        Err(_) => return Value::Null,
    };
    match item.kind {
        "boolean" => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" => json!(true),
            "0" | "false" => json!(false),
            _ => Value::Null,
        },
        "integer" => raw
            .trim()
            .parse::<i64>()
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
        "secret" => {
            if raw.trim().is_empty() {
                Value::Null
            } else {
                json!(SECRET_MASK)
            }
        }
        _ => json!(raw),
    }
}

/// 密钥类只给占位符：`.env` 里有没有配过密钥，不该从面板看得见。
///
/// 未配置的密钥给空串——"没配"和"配了但不给看"是两件事。
fn masked(key: SettingsKey, value: Option<&Value>) -> Value {
    match value {
        Some(value) if key.is_secret() => {
            let configured = value
                .as_str()
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false);
            if configured {
                json!(SECRET_MASK)
            } else {
                json!("")
            }
        }
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

/// 提供方的稳定名字（存库与显示都用它）。
fn provider_name(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::OpenAI => "openai",
        LlmProvider::Gemini => "google",
    }
}

/// 结构化输出模式的规范化：认得的就小写，认不得的退回 `json_schema`。
///
/// `.env` 里手写错一个词不该让整个服务起不来——那是个显示层偏好，不是底线。
fn normalize_structured_output(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "json_object" => "json_object".to_string(),
        _ => "json_schema".to_string(),
    }
}

fn parse_provider(key: SettingsKey, value: &Value) -> Result<LlmProvider> {
    let raw = value
        .as_str()
        .ok_or_else(|| SqlError::Config(format!("{} 必须是字符串", key.as_str())))?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(LlmProvider::OpenAI),
        "google" | "gemini" => Ok(LlmProvider::Gemini),
        _ => Err(SqlError::Config(format!(
            "{} 只能是 openai 或 google",
            key.as_str()
        ))),
    }
}

fn structured_output_value(key: SettingsKey, value: &Value) -> Result<String> {
    let raw = string_value(key, value)?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "json_schema" | "json_object" => Ok(raw.trim().to_ascii_lowercase()),
        _ => Err(SqlError::Config(format!(
            "{} 必须是 json_schema 或 json_object",
            key.as_str()
        ))),
    }
}

fn string_value(key: SettingsKey, value: &Value) -> Result<String> {
    value
        .as_str()
        .map(|text| text.trim().to_string())
        .ok_or_else(|| SqlError::Config(format!("{} 必须是字符串", key.as_str())))
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

    fn base() -> RuntimeSettings {
        RuntimeSettings::from_bootstrap(Config::get())
    }

    #[test]
    fn rejects_invalid_values_and_cross_field_breaks() {
        let base = base();
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

        let mut changes = BTreeMap::new();
        changes.insert("llm_structured_output".to_string(), json!("yaml"));
        assert!(
            base.apply(&changes).is_err(),
            "结构化输出模式必须是两种之一"
        );
    }

    #[test]
    fn normalizes_values_and_returns_typed_snapshot() {
        let base = base();
        let mut changes = BTreeMap::new();
        changes.insert("intent_min_minutes".to_string(), json!(10));
        changes.insert("intent_max_minutes".to_string(), json!(20));
        changes.insert("intent_quiet_hours".to_string(), json!(" 23 - 7 "));
        let (next, normalized) = base.apply(&changes).unwrap();
        assert_eq!(next.intent.min_minutes, 10);
        assert_eq!(next.intent.quiet_hours, (23, 7));
        assert_eq!(normalized["intent_quiet_hours"], json!("23-7"));
    }

    #[test]
    fn schema_covers_every_editable_key_and_the_read_only_ones() {
        let current = base();
        let items = schema(&current);
        let editable: Vec<&str> = items
            .iter()
            .filter(|item| item.editable)
            .map(|item| item.key.as_str())
            .collect();
        let expected: Vec<&str> = SettingsKey::ALL.iter().map(|key| key.as_str()).collect();
        assert_eq!(editable, expected, "可改键应与 SettingsKey::ALL 一一对应");
        assert!(
            items.len() > editable.len(),
            "只读键（通道 / 工具 / 程序本身）也应出现在 schema 里"
        );
        assert!(items
            .iter()
            .all(|item| item.hot_reload != item.restart_required));
    }

    #[test]
    fn schema_is_grouped_in_group_order() {
        let items = schema(&base());
        let groups: Vec<&str> = items.iter().map(|item| item.group.as_str()).collect();
        // 分组顺序与 SettingsGroup::ALL 一致，且组内连续。
        let mut expected: Vec<&str> = Vec::new();
        for group in SettingsGroup::ALL {
            expected.push(group.as_str());
        }
        let mut seen: Vec<&str> = Vec::new();
        for group in groups {
            if seen.last() != Some(&group) {
                assert!(!seen.contains(&group), "分组应连续出现：{group}");
                seen.push(group);
            }
        }
        assert_eq!(seen, expected);
    }

    #[test]
    fn every_key_belongs_to_a_group_and_maps_to_an_env_var() {
        for key in SettingsKey::ALL {
            let group = key.group();
            let upper = key.as_str().to_uppercase();
            assert!(
                group.env_vars().contains(&upper.as_str()),
                "{upper} 不在分组 {:?} 的环境变量清单里",
                group
            );
        }
    }

    #[test]
    fn secret_values_are_masked_in_schema() {
        let items = schema(&base());
        for item in items {
            if item.kind == "secret" {
                // 未配置给空串或 null，已配置给占位符——任何一种都不泄露真值。
                assert!(
                    matches!(item.current, Value::Null)
                        || item.current == json!("")
                        || item.current == json!(SECRET_MASK),
                    "密钥当前值不该是明文：{}",
                    item.current
                );
                assert!(
                    matches!(item.default, Value::Null)
                        || item.default == json!("")
                        || item.default == json!(SECRET_MASK),
                    "密钥缺省值不该是明文：{}",
                    item.default
                );
            }
        }
    }

    #[test]
    fn secret_placeholder_never_leaks_into_the_public_view() {
        let mut base = base();
        base.llm.api_key = "sk-real-secret".to_string();
        let public = base.to_public_map();
        assert_eq!(public["llm_api_key"], json!(SECRET_MASK));
        // 真值只在内部视图里。
        assert_eq!(base.to_map()["llm_api_key"], json!("sk-real-secret"));
    }

    #[test]
    fn provider_and_structured_output_are_validated() {
        let base = base();
        let mut changes = BTreeMap::new();
        changes.insert("llm_provider".to_string(), json!("google"));
        let (next, normalized) = base.apply(&changes).unwrap();
        assert_eq!(next.llm.provider, LlmProvider::Gemini);
        assert_eq!(normalized["llm_provider"], json!("google"));

        let mut changes = BTreeMap::new();
        changes.insert("llm_provider".to_string(), json!("anthropic"));
        assert!(base.apply(&changes).is_err());
    }

    #[test]
    fn endpoint_keys_require_restart_while_model_does_not() {
        assert!(SettingsKey::LlmApiUrl.restart_required());
        assert!(SettingsKey::LlmApiKey.restart_required());
        assert!(SettingsKey::EmbeddingApiUrl.restart_required());
        assert!(!SettingsKey::LlmModel.restart_required());
        assert!(!SettingsKey::IntentEnabled.restart_required());
    }
}
