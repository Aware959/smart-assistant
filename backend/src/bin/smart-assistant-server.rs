use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
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

/// 解析 TLS 证书/私钥：优先取环境变量指定路径，否则回退到运行目录
/// `certs/server.pem` + `certs/server.key`（由 scripts/gen-cert.ps1 生成）。
struct TlsFiles {
    cert: PathBuf,
    key: PathBuf,
}

fn resolve_tls(cfg: &Config) -> Option<TlsFiles> {
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    if let (Some(c), Some(k)) = (&cfg.tls_cert, &cfg.tls_key) {
        candidates.push((PathBuf::from(c), PathBuf::from(k)));
    }
    candidates.push((
        PathBuf::from("certs/server.pem"),
        PathBuf::from("certs/server.key"),
    ));

    candidates
        .into_iter()
        .find(|(cert, key)| cert.exists() && key.exists())
        .map(|(cert, key)| TlsFiles { cert, key })
}

/// 挂载前端静态产物（SPA）：未知路径回退到 index.html，供 API 同源托管。
fn attach_spa(app: Router, dist: &str) -> Router {
    use tower_http::services::{ServeDir, ServeFile};

    let dist = PathBuf::from(dist);
    if !dist.join("index.html").exists() {
        tracing::warn!(dist = %dist.display(), "web_dist 不存在，跳过静态托管");
        return app;
    }

    let index = ServeFile::new(dist.join("index.html"));
    app.fallback_service(
        ServeDir::new(&dist).not_found_service(index),
    )
}

#[tokio::main]
async fn main() {
    // 加载运行目录下的 .env 文件（不存在则静默跳过，环境变量优先）。
    dotenvy::dotenv().ok();

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

    // 配置了 TELEGRAM_BOT_TOKEN 时，后台启动 Telegram 长轮询（与 HTTP 服务同进程）。
    #[cfg(feature = "telegram")]
    if !cfg.telegram_bot_token.trim().is_empty() {
        tracing::info!("TELEGRAM_BOT_TOKEN 已配置，启动 Telegram 长轮询");
        tokio::spawn(smart_assistant::channels::telegram::run(assistant.clone()));
    }

    // 微信 iLink 通道：无 token 且未启用时内部静默跳过；启用后首次启动需扫码。
    #[cfg(feature = "ilink")]
    {
        tokio::spawn(smart_assistant::channels::ilink::run(assistant.clone()));
    }

    // 主动陪伴引擎：PROACTIVE_ENABLED=1 时开启（依赖通道状态做频率/窗口控制）。
    #[cfg(any(feature = "telegram", feature = "ilink"))]
    if cfg.proactive_enabled {
        tracing::info!("PROACTIVE_ENABLED 已开启，启动主动陪伴引擎");
        tokio::spawn(smart_assistant::proactive::run(assistant.clone()));
    }

    // 世界引擎：AI 自身状态持续演进（情绪回落 / 关系降温 / 今日叙事），
    // 与主动推送解耦，始终运行。
    #[cfg(any(feature = "telegram", feature = "ilink"))]
    {
        tokio::spawn(smart_assistant::world::run(assistant.clone()));
    }

    // 构建路由（若配置了前端产物，则同源托管 SPA 静态文件）
    let app = api::build_router(assistant);
    let app = match cfg.web_dist.as_deref() {
        Some(dist) => attach_spa(app, dist),
        None => app,
    };

    let addr: SocketAddr = std::env::var("SMART_ASSISTANT_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string())
        .parse()
        .expect("SMART_ASSISTANT_ADDR 应为 host:port");

    match resolve_tls(cfg) {
        Some(tls) => {
            let rustls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                tls.cert.clone(),
                tls.key.clone(),
            )
            .await
            .expect("failed to load TLS certificate/key");
            tracing::info!(
                addr = %addr,
                cert = %tls.cert.display(),
                "SMART Assistant HTTPS server started"
            );
            axum_server::bind_rustls(addr, rustls)
                .serve(app.into_make_service())
                .await
                .expect("server error");
        }
        None => {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("failed to bind address");
            tracing::info!(addr = %addr, "SMART Assistant HTTP server started（未启用 TLS）");
            axum::serve(listener, app).await.expect("server error");
        }
    }
}