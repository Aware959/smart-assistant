use std::sync::OnceLock;

pub static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: String,
    pub llm_api_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub embedding_api_url: String,
    pub embedding_api_key: String,
    pub embedding_model: String,
    pub embedding_dim: usize,
    /// HTTPS 证书链文件（PEM）。留空时回退到运行目录 `certs/server.pem`。
    pub tls_cert: Option<String>,
    /// HTTPS 私钥文件（PEM）。留空时回退到运行目录 `certs/server.key`。
    pub tls_key: Option<String>,
    /// 前端静态产物目录（SPA）。缺省为 `../frontend/dist`；目录不存在则不挂载。
    pub web_dist: Option<String>,
    /// Telegram Bot Token（@BotFather 获取）。留空则不启动 TG 长轮询。
    pub telegram_bot_token: String,
    /// 允许接入的 Telegram chat id 白名单（逗号分隔）。留空表示不限制。
    pub telegram_allowed_ids: Vec<i64>,
    /// Telegram API 代理（如 http://127.0.0.1:7890）。留空则依次尝试 HTTPS_PROXY / ALL_PROXY。
    pub telegram_proxy: Option<String>,
    /// 启用微信 iLink 通道（扫码登录）。已有 token/会话文件时可不设。
    pub ilink_enabled: bool,
    /// 微信 iLink bot_token（扫码后自动写入会话文件；也可手动设置跳过扫码）。
    pub ilink_bot_token: String,
    /// 微信 iLink 会话文件（保存 bot_token/baseurl，默认 `ilink_session.json`）。
    pub ilink_session_file: String,
    /// 允许接入的微信用户 id 白名单（逗号分隔，形如 xxx@im.wechat）。留空表示不限制。
    pub ilink_allowed_ids: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: "smart_assistant.db".to_string(),
            llm_api_url: "http://127.0.0.1:1234/v1/completions".to_string(),
            llm_api_key: String::new(),
            llm_model: "qwen3.5-9b-uncensored-hauhaucs-aggressive".to_string(),
            embedding_api_url: "http://127.0.0.1:1234/v1/embeddings".to_string(),
            embedding_api_key: String::new(),
            embedding_model: "text-embedding-embeddinggemma-300m".to_string(),
            embedding_dim: 768,
            tls_cert: None,
            tls_key: None,
            web_dist: Some("../frontend/dist".to_string()),
            telegram_bot_token: String::new(),
            telegram_allowed_ids: Vec::new(),
            telegram_proxy: None,
            ilink_enabled: false,
            ilink_bot_token: String::new(),
            ilink_session_file: "ilink_session.json".to_string(),
            ilink_allowed_ids: Vec::new(),
        }
    }
}

impl Config {
    pub fn init_from_env() -> Self {
        Self {
            db_path: std::env::var("SMART_ASSISTANT_DB")
                .unwrap_or_else(|_| "smart_assistant.db".to_string()),
            llm_api_url: std::env::var("LLM_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:1234/v1/completions".to_string()),
            llm_api_key: std::env::var("LLM_API_KEY").unwrap_or_default(),
            llm_model: std::env::var("LLM_MODEL")
                .unwrap_or_else(|_| "google/gemma-4-26b-a4b-qat".to_string()),
            embedding_api_url: std::env::var("EMBEDDING_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:1234/v1/embeddings".to_string()),
            embedding_api_key: std::env::var("EMBEDDING_API_KEY").unwrap_or_default(),
            embedding_model: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-embeddinggemma-300m".to_string()),
            embedding_dim: std::env::var("EMBEDDING_DIM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(768),
            tls_cert: std::env::var("SMART_ASSISTANT_TLS_CERT").ok(),
            tls_key: std::env::var("SMART_ASSISTANT_TLS_KEY").ok(),
            web_dist: std::env::var("SMART_ASSISTANT_WEB_DIST").ok().or_else(|| {
                Some("../frontend/dist".to_string())
            }),
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
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
        }
    }

    pub fn get() -> &'static Config {
        CONFIG.get_or_init(Config::init_from_env)
    }
}
