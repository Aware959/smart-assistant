use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use smart_assistant::api;
use smart_assistant::config::Config;
use smart_assistant::App;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// 双写日志 writer：stdout（控制台）+ 可选文件（`SMART_ASSISTANT_LOG_FILE`）。
/// 只被 tracing 的 writer 线程使用，写失败一律忽略，绝不让日志影响业务。
struct TeeWriter {
    file: Option<std::sync::Mutex<std::fs::File>>,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stdout().write_all(buf);
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.write_all(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stdout().flush();
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

/// 以追加方式打开日志文件，必要时先创建父目录（如 `logs/trace.jsonl` 的 `logs/`）。
fn open_append(path: &str) -> std::io::Result<std::fs::File> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)
}

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
    // 全局 panic hook：把 panic 位置（含线程名）通过 tracing 记录，便于与各阶段日志
    // 对照时间轴定位崩溃环节。默认 hook 同时保留，panic 信息也会打到 stderr。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        tracing::error!(thread = %thread_name, location = %location, "PANIC: {info}");
        default_hook(info);
    }));

    // 加载运行目录下的 .env 文件（不存在则静默跳过，环境变量优先）。
    dotenvy::dotenv().ok();

    // 日志写入走独立 writer 线程（非阻塞通道）：
    // tracing_subscriber 全局写锁 + 一次卡住的 stdout/管道 syscall 曾把 accept
    // 中间件、轮询等所有打日志的线程串行堵死，进程全网冻结。
    // 现在通道写满时只是丢弃日志，任何业务路径都不会再被日志写阻塞。
    // 指定 SMART_ASSISTANT_LOG_FILE 时额外镜像到文件（stdout 仍保留）。
    let file_mirror =
        std::env::var("SMART_ASSISTANT_LOG_FILE").ok().filter(|f| !f.is_empty());
    let mut tee = TeeWriter { file: None };
    if let Some(path) = file_mirror {
        match open_append(&path) {
            Ok(f) => tee.file = Some(std::sync::Mutex::new(f)),
            Err(e) => eprintln!("[log] 打开日志文件失败，仅输出 stdout: {path} ({e})"),
        }
    }
    let (non_blocking, _guard) = tracing_appender::non_blocking(tee);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        // qwen3 等思考型模型流式时先吐 reasoning 空增量，genai 会逐个打 WARN：
        // 压到 error 级，避免噪音（RUST_LOG 手动设置时仍由环境变量说了算）。
        .unwrap_or_else(|_| "info,genai::adapter::adapters::openai::streamer=error".into());
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_filter(env_filter);

    // 可选：链路追踪 JSON 落盘（`SMART_ASSISTANT_TRACE_FILE`）。只收 `target="trace"`
    // 的事件，一行一 JSON，按 trace_id / session_id 过滤即可回放一次请求的全过程。
    let trace_sink = std::env::var("SMART_ASSISTANT_TRACE_FILE")
        .ok()
        .filter(|f| !f.is_empty())
        .and_then(|path| match open_append(&path) {
            Ok(f) => {
                let (writer, guard) = tracing_appender::non_blocking(f);
                Some((writer, guard, path))
            }
            Err(e) => {
                eprintln!("[trace] 打开追踪日志文件失败，仅输出常规日志: {path} ({e})");
                None
            }
        });
    let trace_layer = trace_sink.as_ref().map(|(writer, _, _)| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer.clone())
            .with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("trace", tracing::Level::TRACE),
            )
    });

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(trace_layer)
        .init();
    // 追踪 writer 的 WorkerGuard：必须在整个进程存活期间持有，否则缓冲日志会丢失。
    let _trace_guard = trace_sink.map(|(_, guard, path)| {
        tracing::info!(path = %path, "链路追踪日志已启用（target=trace，JSON 行）");
        guard
    });

    // 诊断心跳：SMART_ASSISTANT_WATCHDOG_FILE 指定时，用一个独立 OS 线程每 5 秒
    // 直接落一行（绕开 tracing）。同时一个 tokio 任务每秒更新计数器；探针据此
    // 区分「tokio 调度器已停摆」与「OS 线程也停了」——冻结类问题的关键观测。
    if let Some(hb_path) =
        std::env::var("SMART_ASSISTANT_WATCHDOG_FILE").ok().filter(|f| !f.is_empty())
    {
        let last_tokio = Arc::new(std::sync::atomic::AtomicU64::new(0));
        {
            let last_tokio = Arc::clone(&last_tokio);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    last_tokio.store(now_ms, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        std::thread::spawn(move || {
            let mut file = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&hb_path)
            {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("watchdog: cannot open {hb_path}: {e}");
                    return;
                }
            };
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let last = last_tokio.load(std::sync::atomic::Ordering::Relaxed);
                let stale_ms = if last == 0 {
                    u64::MAX
                } else {
                    now_ms.saturating_sub(last)
                };
                let line = format!(
                    "hb ts={now_ms} tokio_last_ms={last} stale_ms={stale_ms} os_thread_alive=1\n"
                );
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        });
    }

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
    let app = Arc::new(
        App::new(&cfg.db_path).expect("failed to open database"),
    );
    tracing::info!(
        db = %cfg.db_path,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "database ready"
    );

    // 组件层（telegram / ilink / intent）直接持服务引用走方法调用，
    // web API 路由只拿到主线聊天服务（chat），同实例共用同一 Agent。

    // 配置了 TELEGRAM_BOT_TOKEN 且 TELEGRAM_ENABLED != 0 时，后台启动 Telegram 长轮询。
    #[cfg(feature = "telegram")]
    if cfg.telegram_enabled && !cfg.telegram_bot_token.trim().is_empty() {
        tracing::info!("TELEGRAM_ENABLED + TELEGRAM_BOT_TOKEN 已配置，启动 Telegram 长轮询");
        tokio::spawn(smart_assistant::channels::telegram::run(
            app.chat.clone(),
            app.session.clone(),
            app.tracker.clone(),
        ));
    }

    // 微信 iLink 通道：无 token 且未启用时内部静默跳过；启用后首次启动需扫码。
    #[cfg(feature = "ilink")]
    {
        tokio::spawn(smart_assistant::channels::ilink::run(
            app.chat.clone(),
            app.session.clone(),
            app.tracker.clone(),
        ));
    }

    // 意图判定引擎：INTENT_ENABLED=1 时开启（依赖通道状态做频率/窗口控制）。
    #[cfg(any(feature = "telegram", feature = "ilink"))]
    if cfg.intent_enabled {
        tracing::info!("INTENT_ENABLED 已开启，启动意图判定引擎");
        tokio::spawn(smart_assistant::core::intent::run(
            app.chat.clone(),
            app.session.clone(),
            app.tracker.clone(),
            std::sync::Arc::new(smart_assistant::channels::ChannelsPusher),
        ));
    }

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
                "SMART Assistant HTTPS server started"
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
            tracing::info!(addr = %addr, "SMART Assistant HTTP server started（未启用 TLS）");
            axum::serve(listener, http_app).await.expect("server error");
        }
    }
}