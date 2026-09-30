//! 通道守护进程：telegram / iLink 长轮询 + 意图主动开口。
//!
//! 与 `sa-web`（HTTP 会话主进程）各跑一进程、共享同一个 SQLite 库：
//! 它只负责收消息 → 调主线聊天服务 → 回消息，以及 `INTENT_ENABLED=1`
//! 时按节奏主动开口。同一份配置（.env / smart_assistant.db）下，
//! 可只跑这一个 exe 让 Telegram / 微信通道单独工作，无需启动 HTTP。

use std::sync::Arc;

use smart_assistant::config::Config;
use smart_assistant::utils::runtime::bootstrap;
use smart_assistant::App;

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

    // Telegram 长轮询：TELEGRAM_BOT_TOKEN 非空且 TELEGRAM_ENABLED != 0 时启动。
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
            Arc::new(smart_assistant::channels::ChannelsPusher),
        ));
    }

    tracing::info!("sa-link 通道守护已就绪，Ctrl+C 退出");
    // 无 HTTP 端口可挂，进程靠 ctrl_c 信号保持存活。
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl_c");
    tracing::info!("收到退出信号，sa-link 退出");
}
