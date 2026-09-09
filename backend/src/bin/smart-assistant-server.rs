use std::sync::Arc;

use smart_assistant::api;
use smart_assistant::config::Config;
use smart_assistant::Assistant;

/// 脱敏 API key：只保留首尾四位，空值显示为 <not set>。
fn mask_key(key: &str) -> String {
    if key.is_empty() {
        "<not set>".to_string()
    } else if key.len() <= 10 {
        format!("{}****", &key[..(key.len().min(4))])
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 启动初始化：加载配置（key 脱敏输出）
    let cfg = Config::get();
    tracing::info!(
        db = %cfg.db_path,
        llm_api_url = %cfg.llm_api_url,
        llm_model = %cfg.llm_model,
        llm_api_key = %mask_key(&cfg.llm_api_key),
        "llm config loaded"
    );
    tracing::info!(
        embedding_api_url = %cfg.embedding_api_url,
        embedding_model = %cfg.embedding_model,
        embedding_dim = cfg.embedding_dim,
        embedding_api_key = %mask_key(&cfg.embedding_api_key),
        "embedding config loaded"
    );
    if cfg.llm_api_key.is_empty() {
        tracing::warn!("llm_api_key 未设置：将回退到 OPENAI_API_KEY 环境变量");
    }
    if cfg.embedding_api_key.is_empty() {
        tracing::warn!("embedding_api_key 未设置：将回退到 OPENAI_API_KEY 环境变量");
    }

    // 初始化数据库
    let start = std::time::Instant::now();
    let assistant = Arc::new(
        Assistant::new(&cfg.db_path).expect("failed to open database"),
    );
    tracing::info!(
        db = %cfg.db_path,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "database ready"
    );

    // 构建路由
    let app = api::build_router(assistant);

    let addr = std::env::var("SMART_ASSISTANT_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    tracing::info!(addr = %addr, "SMART Assistant server started");

    axum::serve(listener, app)
        .await
        .expect("server error");
}