//! Zhiwei 运维 CLI：不参与对话，只维护本地服务所需的数据与配置。
//!
//! ```text
//! zhiwei-cli db check
//! zhiwei-cli db rebuild [--force]
//! zhiwei-cli sticker fetch <pack>...
//! zhiwei-cli sticker show
//! zhiwei-cli config get
//! zhiwei-cli config schema
//! zhiwei-cli config set <key> <value>
//! zhiwei-cli config audit [--limit N]
//! ```
//!
//! `db` 与 `sticker` 直接操作本地数据；`config` 只通过 `zhiwei-service`
//! 的 loopback 控制接口读写，CLI 不成为第二个配置写者。

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::Value;

use smart_assistant::config::Config;
use smart_assistant::control::client::ControlClient;
use smart_assistant::core::memory;
use smart_assistant::core::settings::{
    SettingsAuditEntry, SettingsSchemaItem, SettingsUpdate, SettingsView,
};
use smart_assistant::core::sticker::StickerCatalog;
use smart_assistant::db;

const USAGE: &str = "\
用法:
  zhiwei-cli db check
  zhiwei-cli db rebuild [--force]
  zhiwei-cli sticker fetch <贴纸包名>...
  zhiwei-cli sticker show
  zhiwei-cli config get
  zhiwei-cli config schema
  zhiwei-cli config set <key> <value>
  zhiwei-cli config audit [--limit N]
  zhiwei-cli --version";

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--version") {
        println!("{}", smart_assistant::utils::version::display());
        return ExitCode::SUCCESS;
    }

    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [group, sub, rest @ ..] if group == "db" => cmd_db(sub, rest),
        [group, sub, rest @ ..] if group == "sticker" => cmd_sticker(sub, rest),
        [group, sub, rest @ ..] if group == "config" => cmd_config(sub, rest),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_db(sub: &str, args: &[String]) -> Result<(), String> {
    let cfg = Config::get();
    match sub {
        "check" => {
            let db = db::Database::open(&cfg.db_path).map_err(|error| error.to_string())?;
            print_db_status(&db.vec_status().map_err(|error| error.to_string())?);
            Ok(())
        }
        "rebuild" => {
            let force = args.iter().any(|arg| arg == "--force");
            let db = db::Database::open(&cfg.db_path).map_err(|error| error.to_string())?;
            let status = db.vec_status().map_err(|error| error.to_string())?;
            print_db_status(&status);
            if !status.needs_rebuild() && !force {
                println!("\n  签名一致，无需重建。仍要全部重新向量化请加 --force。");
                return Ok(());
            }
            println!("\n  开始按 memories.content 重新向量化...");
            let done = memory::rebuild_all(&db).map_err(|error| error.to_string())?;
            println!("  重建完成：成功写入 {done} 条向量。");
            print_db_status(&db.vec_status().map_err(|error| error.to_string())?);
            Ok(())
        }
        _ => Err(format!("未知 db 子命令：{sub}\n\n{USAGE}")),
    }
}

fn print_db_status(status: &db::VecStatus) {
    println!("\n===== zhiwei-cli db =====");
    println!(
        "  配置签名      : embedding_dim = {} , embedding_model = {}",
        status.configured_dim, status.configured_model
    );
    println!(
        "  存储签名      : embedding_dim = {} , embedding_model = {}",
        status
            .stored_dim
            .map(|dim| dim.to_string())
            .unwrap_or_else(|| "未记录".to_string()),
        status.stored_model.as_deref().unwrap_or("未记录")
    );
    println!(
        "  向量表实际签名: dim = {} , metric = {}",
        status
            .actual_dim
            .map(|dim| dim.to_string())
            .unwrap_or_else(|| "表不存在".to_string()),
        status.actual_metric.as_deref().unwrap_or("表不存在")
    );
    println!("  memories 记录 : {} 条", status.memory_count);
    println!("  memory_vectors: {} 条", status.vector_count);
    if status.needs_rebuild() {
        println!(
            "  状态          : 签名不一致，需要重建（{}）",
            status.diff_summary().unwrap_or_default()
        );
    } else if status.search_usable() {
        println!("  状态          : 正常，记忆检索可用");
    } else {
        println!("  状态          : 表缺失且无签名（全新库由主程序创建）");
    }
}

fn cmd_sticker(sub: &str, args: &[String]) -> Result<(), String> {
    let cfg = Config::get();
    match sub {
        "show" => {
            let catalog = StickerCatalog::load(&cfg.sticker_catalog_file);
            if catalog.is_empty() {
                return Err(format!(
                    "{} 不存在或是空目录（贴纸能力关闭）",
                    cfg.sticker_catalog_file
                ));
            }
            println!("\n===== zhiwei-cli sticker =====");
            println!("  贴纸包  : {}（{}）", catalog.set, catalog.title);
            println!("  拉取时间: {}", catalog.updated_at);
            println!("  选择键  : {}", catalog.keys().join(" "));
            Ok(())
        }
        "fetch" => {
            let packs: Vec<String> = args
                .iter()
                .filter(|arg| !arg.starts_with("--"))
                .cloned()
                .collect();
            if packs.is_empty() {
                return Err(format!("缺少贴纸包名\n\n{USAGE}"));
            }
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|error| format!("无法启动异步运行时：{error}"))?;
            runtime.block_on(fetch_stickers(cfg, &packs))
        }
        _ => Err(format!("未知 sticker 子命令：{sub}\n\n{USAGE}")),
    }
}

async fn fetch_stickers(cfg: &Config, packs: &[String]) -> Result<(), String> {
    let mut merged = StickerCatalog::empty();
    let mut prefix_next = false;
    for pack in packs {
        let one = smart_assistant::channels::telegram::fetch_sticker_set(pack)
            .await
            .map_err(|error| format!("{pack}: {error}"))?;
        println!("  {pack}: {}（{} 张）", one.title, one.stickers.len());
        let (set, title) = (one.set.clone(), one.title.clone());
        let prefix = if prefix_next {
            format!("{set}-")
        } else {
            String::new()
        };
        for (key, file_id) in one.stickers {
            merged
                .stickers
                .entry(format!("{prefix}{key}"))
                .or_insert(file_id);
        }
        if merged.set.is_empty() {
            merged.set = set;
            merged.title = title;
        }
        prefix_next = true;
    }
    if merged.stickers.is_empty() {
        return Err("没有拉到任何贴纸".to_string());
    }
    merged.updated_at = chrono::Utc::now().to_rfc3339();
    merged.save(&cfg.sticker_catalog_file)?;
    println!(
        "\n  已写入 {}（{} 个选择键）",
        cfg.sticker_catalog_file,
        merged.stickers.len()
    );
    println!("  可选键: {}", merged.keys().join(" "));
    println!("  重启 zhiwei-service 后生效；手改 JSON 的键会在下次 fetch 时被覆盖。");
    Ok(())
}

fn cmd_config(sub: &str, args: &[String]) -> Result<(), String> {
    let cfg = Config::get();
    let client = ControlClient::from_config(cfg)?;
    match sub {
        "get" => {
            let view: SettingsView = client.get("/control/v1/settings")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&view).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        "schema" => {
            let schema: Vec<SettingsSchemaItem> = client.get("/control/v1/settings/schema")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&schema).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        "audit" => {
            let limit = args
                .windows(2)
                .find(|pair| pair[0] == "--limit")
                .and_then(|pair| pair[1].parse::<usize>().ok())
                .unwrap_or(50);
            let audit: Vec<SettingsAuditEntry> =
                client.get(&format!("/control/v1/settings/audit?limit={limit}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&audit).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        "set" => {
            if args.len() != 2 {
                return Err(format!(
                    "用法：zhiwei-cli config set <key> <value>\n\n{USAGE}"
                ));
            }
            let view: SettingsView = client.get("/control/v1/settings")?;
            let value = parse_cli_value(&args[1]);
            let changes = BTreeMap::from([(args[0].clone(), value)]);
            let result: SettingsUpdate = client.patch(
                "/control/v1/settings",
                &serde_json::json!({
                    "expectedRevision": view.revision,
                    "operator": "cli",
                    "changes": changes,
                }),
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        _ => Err(format!("未知 config 子命令：{sub}\n\n{USAGE}")),
    }
}

fn parse_cli_value(raw: &str) -> Value {
    if let Ok(value) = raw.parse::<bool>() {
        return Value::Bool(value);
    }
    if let Ok(value) = raw.parse::<i64>() {
        return Value::Number(value.into());
    }
    if let Ok(value) = raw.parse::<f64>() {
        if let Some(number) = serde_json::Number::from_f64(value) {
            return Value::Number(number);
        }
    }
    Value::String(raw.to_string())
}
