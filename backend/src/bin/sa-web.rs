use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use smart_assistant::api;
use smart_assistant::config::Config;
use smart_assistant::utils::runtime::{bootstrap, mask_key};
use smart_assistant::App;

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
    app.fallback_service(ServeDir::new(&dist).not_found_service(index))
}

#[tokio::main]
async fn main() {
    // 启动脚手架：panic hook + dotenv + 文本/链路日志 + 心跳探针。
    let _rt = bootstrap();

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
    let app = Arc::new(App::new(&cfg.db_path).expect("failed to open database"));
    tracing::info!(
        db = %cfg.db_path,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "database ready"
    );

    // 构建路由（若配置了前端产物，则同源托管 SPA 静态文件）
    let http_app = api::build_router(app.chat.clone());
    let http_app = match cfg.web_dist.as_deref() {
        Some(dist) => attach_spa(http_app, dist),
        None => http_app,
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
                "sa-web HTTPS server started"
            );
            axum_server::bind_rustls(addr, rustls)
                .serve(http_app.into_make_service())
                .await
                .expect("server error");
        }
        None => {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("failed to bind address");
            tracing::info!(addr = %addr, "sa-web HTTP server started（未启用 TLS）");
            axum::serve(listener, http_app).await.expect("server error");
        }
    }
}
