use std::io::{self, Write};

use smart_assistant::config::Config;
use smart_assistant::{App, ChatInput};

/// SMART Assistant 命令行入口。
///
/// - 无参数：进入交互式 REPL（流式输出到终端）；
/// - `sa-cli "一句话"`：单发问答，打印完整回复后退出。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 加载运行目录下的 .env 文件（不存在则静默跳过，环境变量优先）。
    dotenvy::dotenv().ok();

    // 初始化日志（与 sa-link / sa-web 同一个 bootstrap）。
    // 不接的话 CLI 完全不输出 tracing——工具调用入参、LLM 流式统计这些
    // 就只能在 Telegram 端看，排查"模型到底调了什么"时没有第二只眼睛。
    // guard 必须活到 main 结束，否则非阻塞写线程会被提前拆掉、丢日志。
    let _runtime = smart_assistant::utils::runtime::bootstrap();

    // Ctrl+C：安装处理器后用 exit(0) 干净退出（Windows 下默认是 STATUS_CONTROL_C_EXIT 非正常终止）。
    ctrlc::set_handler(|| {
        println!("\n[Ctrl+C] 再见");
        std::process::exit(0);
    })?;

    let cfg = Config::get();
    let app = App::new(&cfg.db_path)?;
    let chat = app.chat;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let message = args.first().cloned().filter(|m| !m.trim().is_empty());

    if let Some(m) = message {
        // 单发模式
        let input = ChatInput {
            message: m,
            session_id: None,
            history: vec![],
            history_count: None,
            user_time: None,
        };
        // 回调收到的是**一条完整消息**（不再逐字增量）：每次调用即一行。
        // 注意 `on_message` 在 `chat_stream` 返回前就已全部触发，这里不能再打
        // `out.reply`——那会把整轮内容重复输出一遍。
        match chat.chat_stream(&input, |msg| {
            println!("{msg}");
            io::stdout().flush().ok();
        }) {
            Ok(out) => {
                println!("[会话 {}]", out.session_id);
                // 回复已输出，再补记忆沉淀（CLI 无并发压力，同步即可）。
                if let Some(mem) =
                    chat.settle_memory(&out.session_id, &out.user_message_id, &input.message)
                {
                    println!("[已记住] {}", mem.content);
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // 交互式 REPL
    println!("SMART Assistant CLI（输入 exit 或 Ctrl+C 退出）");
    println!("发送消息即开始对话，回复将实时输出；上下文自动按当天时间线携带（本市凌晨 6 点为一日的开始）。");

    let stdin = io::stdin();
    let mut session_id: Option<String> = None;
    loop {
        print!("你> ");
        io::stdout().flush()?;

        let mut line = String::new();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }
        if text.eq_ignore_ascii_case("exit") {
            break;
        }

        let input = ChatInput {
            message: text,
            session_id: session_id.clone(),
            history: vec![],
            history_count: None,
            user_time: None,
        };
        match chat.chat_stream(&input, |msg| {
            println!("{msg}");
        }) {
            Ok(out) => {
                session_id = Some(out.session_id.clone());
                // 回复已输出，再补记忆沉淀（CLI 无并发压力，同步即可）。
                if let Some(mem) =
                    chat.settle_memory(&out.session_id, &out.user_message_id, &input.message)
                {
                    println!("[已记住] {}", mem.content);
                }
                println!("[会话 {}]\n", out.session_id);
            }
            Err(e) => {
                eprintln!("\nerror: {e}");
            }
        }
    }
    Ok(())
}
