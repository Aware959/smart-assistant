//! 通道守护进程：telegram / iLink 长轮询 + 意图主动开口。
//!
//! 与 `sa-web`（HTTP 会话主进程）各跑一进程、共享同一个 SQLite 库：
//! 它只负责收消息 → 调主线聊天服务 → 回消息，以及 `INTENT_ENABLED=1`
//! 时按节奏主动开口。同一份配置（.env / smart_assistant.db）下，
//! 可只跑这一个 exe 让 Telegram / 微信通道单独工作，无需启动 HTTP。
//!
//! 退出语义（Ctrl+C / Ctrl+Break）：
//! - 第一次信号：触发优雅收尾——abort 各轮询循环任务、按句柄有界等待
//!   （每个任务在下一个 await 点自然结束，同步段如 SQLite 写入会跑完不打断）；
//! - 第二次信号 或 收尾超时（[`SHUTDOWN_GRACE`]）：强制退出。阻塞线程池里的
//!   LLM 调用无法中断（tokio 语义），进程级用超时兜底，不再无限等待。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use smart_assistant::config::Config;
use smart_assistant::utils::runtime::bootstrap;
use smart_assistant::App;

/// 优雅收尾上限：超过直接强制退出（阻塞线程池里的 LLM 调用等不到结果时兜底）。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// 退出信号计数：第一次触发优雅收尾，第二次直接强制退出。
static EXIT_SIGNAL_COUNT: AtomicU32 = AtomicU32::new(0);

#[tokio::main]
async fn main() {
    // 启动脚手架：panic hook + dotenv + 文本/链路日志 + 心跳探针。
    let _rt = bootstrap();

    let cfg = Config::get();

    // 初始化数据库（与 sa-web 共享同一个 smart_assistant.db）。
    let start = std::time::Instant::now();
    let app = Arc::new(App::new(&cfg.db_path).expect("failed to open database"));
    tracing::info!(
        db = %cfg.db_path,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "database ready"
    );

    // 各长驻循环的句柄：退出时统一 abort 收尾（不再依赖 runtime drop 一把梭）。
    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Telegram 长轮询：TELEGRAM_BOT_TOKEN 非空且 TELEGRAM_ENABLED != 0 时启动。
    #[cfg(feature = "telegram")]
    if cfg.telegram_enabled && !cfg.telegram_bot_token.trim().is_empty() {
        tracing::info!("TELEGRAM_ENABLED + TELEGRAM_BOT_TOKEN 已配置，启动 Telegram 长轮询");
        handles.push(tokio::spawn(smart_assistant::channels::telegram::run(
            app.chat.clone(),
            app.session.clone(),
        )));
        // 失败轮次重试扫表：与长轮询并列，静默重试待重试队列里的轮次。
        handles.push(tokio::spawn(
            smart_assistant::channels::telegram::run_retry_sweep(
                app.chat.clone(),
                app.session.clone(),
            ),
        ));
    }

    // 微信 iLink 通道：无 token 且未启用时内部静默跳过；启用后首次启动需扫码。
    #[cfg(feature = "ilink")]
    {
        handles.push(tokio::spawn(smart_assistant::channels::ilink::run(
            app.chat.clone(),
            app.session.clone(),
        )));
    }

    // 意图判定引擎：INTENT_ENABLED=1 时开启（依赖通道状态做频率/窗口控制）。
    #[cfg(any(feature = "telegram", feature = "ilink"))]
    if cfg.intent_enabled {
        tracing::info!("INTENT_ENABLED 已开启，启动意图判定引擎");
        handles.push(tokio::spawn(smart_assistant::core::intent::run(
            app.chat.clone(),
            app.session.clone(),
            Arc::new(smart_assistant::channels::ChannelsPusher),
        )));
    }

    tracing::info!("sa-link 通道守护已就绪，Ctrl+C / Ctrl+Break 退出");

    // 退出信号：ctrlc 在 Windows 下同时捕获 Ctrl+C 与 Ctrl+Break（tokio 的
    // `signal::ctrl_c` 只认 Ctrl+C）。第一次信号走优雅收尾，第二次强制退出。
    let (sig_tx, mut sig_rx) = tokio::sync::mpsc::channel::<()>(1);
    ctrlc::set_handler(move || {
        let n = EXIT_SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 2 {
            eprintln!("[sa-link] 收到第二次退出信号，强制退出");
            std::process::exit(130);
        }
        let _ = sig_tx.try_send(());
    })
    .expect("failed to install ctrl_c/ctrl_break handler");

    sig_rx.recv().await;
    tracing::info!("收到退出信号，开始收尾");

    // 强制退出兜底：放 OS 线程而非 tokio 任务——runtime drop 在等阻塞线程池时
    // worker 已不再调度 tokio 任务，OS 线程不受影响，超时一定会响。
    std::thread::spawn(|| {
        std::thread::sleep(SHUTDOWN_GRACE);
        eprintln!("[sa-link] 收尾超时，强制退出");
        std::process::exit(130);
    });

    // 各循环都是无限任务：先 abort，再按句柄有界等待。abort 让任务在下一个
    // await 点结束——同步段（如 SQLite 写入）会自然跑完，不会强插。
    for h in &handles {
        h.abort();
    }
    let deadline = tokio::time::sleep(SHUTDOWN_GRACE);
    tokio::pin!(deadline);
    for h in handles {
        tokio::select! {
            _ = h => {}
            _ = &mut deadline => break,
        }
    }

    tracing::info!("sa-link 退出完毕");
}
