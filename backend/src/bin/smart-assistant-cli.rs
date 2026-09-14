use std::io::{self, Write};

use smart_assistant::config::Config;
use smart_assistant::{Assistant, ChatInput};

/// SMART Assistant 命令行入口。
///
/// - 无参数：进入交互式 REPL（流式输出到终端）；
/// - `smart-assistant-cli "一句话"`：单发问答，打印完整回复后退出。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 加载运行目录下的 .env 文件（不存在则静默跳过，环境变量优先）。
    dotenvy::dotenv().ok();

    // Ctrl+C：安装处理器后用 exit(0) 干净退出（Windows 下默认是 STATUS_CONTROL_C_EXIT 非正常终止）。
    ctrlc::set_handler(|| {
        println!("\n[Ctrl+C] 再见");
        std::process::exit(0);
    })?;

    let cfg = Config::get();
    let assistant = Assistant::new(&cfg.db_path)?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let message = args.first().cloned().filter(|m| !m.trim().is_empty());

    if let Some(m) = message {
        // 单发模式
        match assistant.chat_stream(
            &ChatInput {
                message: m,
                session_id: None,
                history: vec![],
                history_count: None,
                user_time: None,
            },
            |delta| {
                print!("{delta}");
                io::stdout().flush().ok();
            },
        ) {
            Ok(out) => {
                println!();
                println!("[会话 {}]", out.session_id);
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
    println!("发送消息即开始对话，回复将实时输出；上下文自动携带最近 6 条消息。");

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

        match assistant.chat_stream(
            &ChatInput {
                message: text,
                session_id: session_id.clone(),
                history: vec![],
                history_count: None,
                user_time: None,
            },
            |delta| {
                print!("{delta}");
                io::stdout().flush().ok();
            },
        ) {
            Ok(out) => {
                println!();
                session_id = Some(out.session_id.clone());
                println!("[会话 {}]\n", out.session_id);
            }
            Err(e) => {
                eprintln!("\nerror: {e}");
            }
        }
    }
    Ok(())
}