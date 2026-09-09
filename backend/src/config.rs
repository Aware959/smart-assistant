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
                .unwrap_or_else(|_| "qwen3.5-9b-uncensored-hauhaucs-aggressive".to_string()),
            embedding_api_url: std::env::var("EMBEDDING_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:1234/v1/embeddings".to_string()),
            embedding_api_key: std::env::var("EMBEDDING_API_KEY").unwrap_or_default(),
            embedding_model: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-embeddinggemma-300m".to_string()),
            embedding_dim: std::env::var("EMBEDDING_DIM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(768),
        }
    }

    pub fn get() -> &'static Config {
        CONFIG.get_or_init(Config::init_from_env)
    }
}
