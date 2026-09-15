use std::sync::OnceLock;

pub static CONFIG: OnceLock<Config> = OnceLock::new();

/// LLM/embedding 依赖的模型提供方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    /// OpenAI 兼容端点（默认）：LM Studio / llama.cpp / Groq / DeepSeek / Moonshot 等。
    OpenAI,
    /// Google AI Studio Gemini 原生协议（generativelanguage.googleapis.com）。
    Gemini,
}

impl LlmProvider {
    /// `LLM_PROVIDER` 环境变量解析：`google`/`gemini` → Gemini，其余 → OpenAI 兼容。
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("gemini") || s.eq_ignore_ascii_case("google") {
            LlmProvider::Gemini
        } else {
            LlmProvider::OpenAI
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: String,
    /// LLM 提供方：openai（默认，OpenAI 兼容端点：LM Studio / llama.cpp / Groq / DeepSeek 等）
    /// 或 google（Google AI Studio Gemini）。
    pub llm_provider: LlmProvider,
    pub llm_api_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    /// 记忆提取专用模型：留空则复用 llm_model。推荐用更强的模型负责
    /// 记忆判定/事实化，对话模型只管聊天（两者可分离）。
    pub llm_extract_model: String,
    /// 结构化输出的请求模式：json_schema（默认，OpenAI/Gemini/本地 llama.cpp 系）
    /// 或 json_object（DeepSeek 等只支持宽松 JSON 模式的端点）。
    pub llm_structured_output: String,
    /// 角色设定（人设/性格/喜欢…）。非空时每一轮对话都以硬约束注入 system 提示词。
    pub persona: String,
    pub embedding_api_url: String,
    pub embedding_api_key: String,
    pub embedding_model: String,
    pub embedding_dim: usize,
    /// 记忆召回的相似度阈值（欧氏距离，≤ 该值才入选；越严越少）。过放宽会
    /// 把无关记忆每轮都注入提示词，过严会"失忆"。需随 embedding 模型微调。
    pub memory_recall_threshold: f32,
    /// 记忆去重阈值：新记忆与库中某条距离 ≤ 该值时视为同一事实，不新增、
    /// 只刷新 updated_at。只有高度近似才算重复，避免误杀同主题的不同事实。
    pub memory_dedup_threshold: f32,
    /// 短期记忆（short）存活天数：如"身体不舒服"这类过期信息。
    pub memory_short_ttl_days: i64,
    /// 意向记忆（intent）存活天数：如"打算下个月去看电影"这类计划。
    pub memory_intent_ttl_days: i64,
    /// HTTPS 证书链文件（PEM）。留空时回退到运行目录 `certs/server.pem`。
    pub tls_cert: Option<String>,
    /// HTTPS 私钥文件（PEM）。留空时回退到运行目录 `certs/server.key`。
    pub tls_key: Option<String>,
    /// 前端静态产物目录（SPA）。缺省为 `../frontend/dist`；目录不存在则不挂载。
    pub web_dist: Option<String>,
    /// Telegram Bot Token（@BotFather 获取）。留空则不启动 TG 长轮询。
    pub telegram_bot_token: String,
    /// Telegram 通道硬开关：`0` 明确不启动 Telegram；缺省视为开启（仍要求 token 非空）。
    pub telegram_enabled: bool,
    /// 允许接入的 Telegram chat id 白名单（逗号分隔）。留空表示不限制。
    pub telegram_allowed_ids: Vec<i64>,
    /// Telegram API 代理（如 http://127.0.0.1:7890）。留空则依次尝试 HTTPS_PROXY / ALL_PROXY。
    pub telegram_proxy: Option<String>,
    /// 启用微信 iLink 通道（硬开关）：`1` 才连接（有 token 直连，无 token 则扫码登录）；`0`/缺省不启动。
    pub ilink_enabled: bool,
    /// 微信 iLink bot_token（扫码后自动写入会话文件；也可手动设置跳过扫码）。
    pub ilink_bot_token: String,
    /// 微信 iLink 会话文件（保存 bot_token/baseurl，默认 `ilink_session.json`）。
    pub ilink_session_file: String,
    /// 允许接入的微信用户 id 白名单（逗号分隔，形如 xxx@im.wechat）。留空表示不限制。
    pub ilink_allowed_ids: Vec<String>,
    /// iLink 的 context_token 新鲜窗口（小时）：超过该时长的 token 视为过期，
    /// 期间需用户再发消息才能刷新（官方表现为约 24h / 10 次主动外发）。
    pub ilink_context_max_age_hours: i64,
    /// 主动陪伴总开关。
    pub proactive_enabled: bool,
    /// 主动发送的随机间隔最小值（分钟）。
    pub proactive_min_minutes: i64,
    /// 主动发送的随机间隔最大值（分钟）。
    pub proactive_max_minutes: i64,
    /// 安静时段（本机时间，HH-HH 半开区间，如 "23-7" 表示 23:00~6:59 不主动；
    /// 起止相同视为全天不安静）。
    pub proactive_quiet_hours: (u32, u32),
    /// 每用户每天的主动消息上限。
    pub proactive_daily_limit: u32,
    /// 距用户最后一次说话超过多少小时后才可能被主动联系（避免打断热聊）。
    pub proactive_min_silence_hours: i64,
    /// 距用户最后一次说话超过多少天还未回复则不再主动打扰（避免骚扰沉睡用户）。
    pub proactive_max_idle_days: i64,
    /// 每轮回复后最多连续追加的语句数：话题有延展性时 AI 会像真人一样接着说
    /// （倾诉/吐槽/讨论进行中）；设为 0 关闭该能力。
    pub proactive_max_followups: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: "smart_assistant.db".to_string(),
            llm_provider: LlmProvider::OpenAI,
            llm_api_url: "http://127.0.0.1:1234/v1/completions".to_string(),
            llm_api_key: String::new(),
            llm_model: "qwen3.5-9b-uncensored-hauhaucs-aggressive".to_string(),
            llm_extract_model: String::new(),
            llm_structured_output: "json_schema".to_string(),
            persona: String::new(),
            embedding_api_url: "http://127.0.0.1:1234/v1/embeddings".to_string(),
            embedding_api_key: String::new(),
            embedding_model: "text-embedding-embeddinggemma-300m".to_string(),
            embedding_dim: 768,
            memory_recall_threshold: 0.9,
            memory_dedup_threshold: 0.25,
            memory_short_ttl_days: 7,
            memory_intent_ttl_days: 90,
            tls_cert: None,
            tls_key: None,
            web_dist: Some("../frontend/dist".to_string()),
            telegram_bot_token: String::new(),
            telegram_enabled: true,
            telegram_allowed_ids: Vec::new(),
            telegram_proxy: None,
            ilink_enabled: false,
            ilink_bot_token: String::new(),
            ilink_session_file: "ilink_session.json".to_string(),
            ilink_allowed_ids: Vec::new(),
            ilink_context_max_age_hours: 12,
            proactive_enabled: false,
            proactive_min_minutes: 45,
            proactive_max_minutes: 180,
            proactive_quiet_hours: (23, 7),
            proactive_daily_limit: 8,
            proactive_min_silence_hours: 2,
            proactive_max_idle_days: 7,
            proactive_max_followups: 3,
        }
    }
}

impl Config {
    pub fn init_from_env() -> Self {
        Self {
            db_path: std::env::var("SMART_ASSISTANT_DB")
                .unwrap_or_else(|_| "smart_assistant.db".to_string()),
            llm_provider: std::env::var("LLM_PROVIDER")
                .map(|v| LlmProvider::parse(&v))
                .unwrap_or(LlmProvider::OpenAI),
            llm_api_url: std::env::var("LLM_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:1234/v1/completions".to_string()),
            llm_api_key: std::env::var("LLM_API_KEY").unwrap_or_default(),
            llm_model: std::env::var("LLM_MODEL")
                .unwrap_or_else(|_| "google/gemma-4-26b-a4b-qat".to_string()),
            llm_extract_model: std::env::var("LLM_EXTRACT_MODEL").unwrap_or_default(),
            llm_structured_output: std::env::var("LLM_STRUCTURED_OUTPUT")
                .unwrap_or_else(|_| "json_schema".to_string()),
            persona: std::env::var("PERSONA").unwrap_or_default(),
            embedding_api_url: std::env::var("EMBEDDING_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:1234/v1/embeddings".to_string()),
            embedding_api_key: std::env::var("EMBEDDING_API_KEY").unwrap_or_default(),
            embedding_model: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-embeddinggemma-300m".to_string()),
            embedding_dim: std::env::var("EMBEDDING_DIM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(768),
            memory_recall_threshold: std::env::var("MEMORY_RECALL_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.9),
            memory_dedup_threshold: std::env::var("MEMORY_DEDUP_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.25),
            memory_short_ttl_days: std::env::var("MEMORY_SHORT_TTL_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            memory_intent_ttl_days: std::env::var("MEMORY_INTENT_TTL_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(90),
            tls_cert: std::env::var("SMART_ASSISTANT_TLS_CERT").ok(),
            tls_key: std::env::var("SMART_ASSISTANT_TLS_KEY").ok(),
            web_dist: std::env::var("SMART_ASSISTANT_WEB_DIST").ok().or_else(|| {
                Some("../frontend/dist".to_string())
            }),
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
            telegram_enabled: std::env::var("TELEGRAM_ENABLED")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true),
            telegram_allowed_ids: std::env::var("TELEGRAM_ALLOWED_IDS")
                .ok()
                .map(|s| {
                    s.split(',')
                        .filter_map(|p| p.trim().parse::<i64>().ok())
                        .collect()
                })
                .unwrap_or_default(),
            telegram_proxy: std::env::var("TELEGRAM_PROXY").ok(),
            ilink_enabled: std::env::var("ILINK_ENABLED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            ilink_bot_token: std::env::var("ILINK_BOT_TOKEN").unwrap_or_default(),
            ilink_session_file: std::env::var("ILINK_SESSION_FILE")
                .unwrap_or_else(|_| "ilink_session.json".to_string()),
            ilink_allowed_ids: std::env::var("ILINK_ALLOWED_IDS")
                .ok()
                .map(|s| {
                    s.split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            ilink_context_max_age_hours: std::env::var("ILINK_CONTEXT_MAX_AGE_HOURS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(12),
            proactive_enabled: std::env::var("PROACTIVE_ENABLED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            proactive_min_minutes: std::env::var("PROACTIVE_MIN_MINUTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(45),
            proactive_max_minutes: std::env::var("PROACTIVE_MAX_MINUTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(180),
            proactive_quiet_hours: parse_quiet_hours(
                std::env::var("PROACTIVE_QUIET_HOURS").unwrap_or_default().as_str(),
            ),
            proactive_daily_limit: std::env::var("PROACTIVE_DAILY_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            proactive_min_silence_hours: std::env::var("PROACTIVE_MIN_SILENCE_HOURS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            proactive_max_idle_days: std::env::var("PROACTIVE_MAX_IDLE_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            proactive_max_followups: std::env::var("PROACTIVE_MAX_FOLLOWUPS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
        }
    }

    pub fn get() -> &'static Config {
        CONFIG.get_or_init(Config::init_from_env)
    }
}

/// 解析 "HH-HH" 形式的安静时段；非法或缺失回退 (0, 7)。
fn parse_quiet_hours(v: &str) -> (u32, u32) {
    let mut iter = v.split('-').map(|p| p.trim().parse::<u32>().ok());
    match (iter.next(), iter.next()) {
        (Some(Some(a)), Some(Some(b))) if a <= 23 && b <= 23 => (a, b),
        _ => (23, 7),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_parsing() {
        assert_eq!(parse_quiet_hours("23-7"), (23, 7));
        assert_eq!(parse_quiet_hours("0-23"), (0, 23));
        assert_eq!(parse_quiet_hours(""), (23, 7));
        assert_eq!(parse_quiet_hours("abc"), (23, 7));
        assert_eq!(parse_quiet_hours("3"), (23, 7));
        assert_eq!(parse_quiet_hours("26-7"), (23, 7));
        assert_eq!(parse_quiet_hours("7-7"), (7, 7));
    }
}
