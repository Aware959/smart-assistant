//! 进程运行时启动脚手架：panic hook / dotenv / 文本日志 / 链路追踪 / 心跳探针。
//!
//! `sa-web`（HTTP 会话）与 `sa-link`（通道守护）共用同一套启动逻辑，
//! 避免两个 bin 各自重复一段 150 行的初始化。约定只在 `#[tokio::main]`
//! 的入口里调用 [`bootstrap`]，返回的 [`Runtime`] 必须持有到进程退出
//! （其内部保存 tracing 非阻塞 writer 的 WorkerGuard，一旦 drop 会冲刷并
//! 关闭日志通道，后续日志直接丢失）。

use std::io::Write;
use std::sync::Arc;

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
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// 脱敏 API key：只保留首尾四位，空值显示为 <not set>。
pub fn mask_key(key: &str) -> String {
    if key.is_empty() {
        "<not set>".to_string()
    } else if key.len() <= 10 {
        format!("{}****", &key[..(key.len().min(4))])
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

/// 运行时脚手架返回的句柄：保存非阻塞 writer 的 WorkerGuard。
/// 任何 bin 在 `main` 里都必须持有到进程结束。
pub struct Runtime {
    _log_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    _trace_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// 完成启动初始化：加载 `.env`、安装 panic hook、初始化文本日志 /
/// 链路追踪 JSON（`SMART_ASSISTANT_TRACE_FILE`）、按需启动心跳探针。
/// 必须在 tokio 运行时内调用（内部有 `tokio::spawn`）。
pub fn bootstrap() -> Runtime {
    // 加载运行目录下的 .env 文件（不存在则静默跳过，环境变量优先）。
    dotenvy::dotenv().ok();

    // 全局 panic hook：把 panic 位置（含线程名）通过 tracing 记录，便于与各阶段
    // 日志对照时间轴定位崩溃环节。默认 hook 同时保留，panic 信息也会打到 stderr。
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

    // 日志写入走独立 writer 线程（非阻塞通道）：
    // tracing_subscriber 全局写锁 + 一次卡住的 stdout/管道 syscall 曾把 accept
    // 中间件、轮询等所有打日志的线程串行堵死，进程全网冻结。
    // 现在通道写满时只是丢弃日志，任何业务路径都不会再被日志写阻塞。
    // 指定 SMART_ASSISTANT_LOG_FILE 时额外镜像到文件（stdout 仍保留）。
    let file_mirror = std::env::var("SMART_ASSISTANT_LOG_FILE")
        .ok()
        .filter(|f| !f.is_empty());
    let mut tee = TeeWriter { file: None };
    if let Some(path) = file_mirror {
        match open_append(&path) {
            Ok(f) => tee.file = Some(std::sync::Mutex::new(f)),
            Err(e) => eprintln!("[log] 打开日志文件失败，仅输出 stdout: {path} ({e})"),
        }
    }
    let (log_writer, log_guard) = tracing_appender::non_blocking(tee);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        // qwen3 等思考型模型流式时先吐 reasoning 空增量，genai 会逐个打 WARN：
        // 压到 error 级，避免噪音（RUST_LOG 手动设置时仍由环境变量说了算）。
        .unwrap_or_else(|_| "info,genai::adapter::adapters::openai::streamer=error".into());
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(log_writer)
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
    // 追踪 writer 的 WorkerGuard：必须在整个进程存活期间持有，否则缓冲日志会丢失。
    let trace_guard = match trace_sink {
        Some((writer, guard, path)) => {
            let layer = tracing_subscriber::fmt::layer()
                .json()
                .with_writer(writer)
                .with_filter(
                    tracing_subscriber::filter::Targets::new()
                        .with_target("trace", tracing::Level::TRACE),
                );
            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(layer)
                .init();
            tracing::info!(path = %path, "链路追踪日志已启用（target=trace，JSON 行）");
            Some(guard)
        }
        None => {
            tracing_subscriber::registry().with(fmt_layer).init();
            None
        }
    };

    // 诊断心跳：SMART_ASSISTANT_WATCHDOG_FILE 指定时，用一个独立 OS 线程每 5 秒
    // 直接落一行（绕开 tracing）。同时一个 tokio 任务每秒更新计数器；探针据此
    // 区分「tokio 调度器已停摆」与「OS 线程也停了」——冻结类问题的关键观测。
    if let Some(hb_path) = std::env::var("SMART_ASSISTANT_WATCHDOG_FILE")
        .ok()
        .filter(|f| !f.is_empty())
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

    Runtime {
        _log_guard: Some(log_guard),
        _trace_guard: trace_guard,
    }
}
