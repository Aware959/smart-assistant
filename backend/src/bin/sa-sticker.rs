/// 贴纸（表情包）目录维护工具：把公开贴纸包拉成「选择键 → file_id」的 JSON。
///
/// `file_id` 是 **bot 专属**的——同一张贴纸换个 bot 就发不出去——所以这份目录
/// 只能由当前 bot 自己去 `getStickerSet` 现场拉，是缓存不是配置；换 bot 或贴纸包
/// 被删后需重拉。拉取要能连上 api.telegram.org（代理沿用 `TELEGRAM_PROXY`）。
///
/// 用法：
///   sa-sticker fetch <贴纸包名>...   拉取并覆盖写入目录文件（可一次给多个包）
///   sa-sticker show                  打印当前目录
///
/// 选择键取每张贴纸的 `emoji`；同 emoji 的重复贴纸自动加 `2`/`3` 区分。键不合适
/// 直接手改 JSON 里的键即可，模型只认改后的键——但**重拉会覆盖手改**。
///
/// 注意：写入后需重启 `sa-link` 才生效（Agent 在启动时读一次目录）。
use std::process::ExitCode;

use smart_assistant::config::Config;
use smart_assistant::core::sticker::StickerCatalog;

fn banner(title: &str) {
    println!("\n===== sa-sticker: {title} =====");
}

fn usage() {
    println!(
        "用法:\n  sa-sticker fetch <贴纸包名>...  拉取贴纸包并写入目录文件（覆盖）\n  sa-sticker show                 打印当前目录"
    );
}

/// 拉取若干个包并合并成一份目录。
///
/// 多包时第二个及以后的键加 `{包名}-` 前缀：不同包常有同 emoji，不前缀就会互相
/// 覆盖。单包（常规用法）的键保持干净。结果**整体覆盖**文件，要合包就得一次给全。
async fn cmd_fetch(cfg: &Config, packs: &[String]) -> Result<(), String> {
    banner("fetch");
    let mut merged = StickerCatalog::empty();
    let mut prefix_next = false;

    for pack in packs {
        let one = smart_assistant::channels::telegram::fetch_sticker_set(pack)
            .await
            .map_err(|e| format!("{pack}: {e}"))?;
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
    println!("  重启 sa-link 后生效；手改 JSON 的键会在下次 fetch 时被覆盖。");
    Ok(())
}

fn cmd_show(cfg: &Config) -> ExitCode {
    banner("show");
    let catalog = StickerCatalog::load(&cfg.sticker_catalog_file);
    if catalog.is_empty() {
        println!(
            "  {} 不存在或是空目录（贴纸能力关闭）。",
            cfg.sticker_catalog_file
        );
        println!("  拉一个包: sa-sticker fetch <贴纸包名>");
        return ExitCode::FAILURE;
    }
    println!("  贴纸包  : {}（{}）", catalog.set, catalog.title);
    println!("  拉取时间: {}", catalog.updated_at);
    println!("  选择键  : {}", catalog.keys().join(" "));
    ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = Config::get();

    match args.first().map(String::as_str) {
        Some("fetch") => {
            let packs: Vec<String> = args[1..]
                .iter()
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .collect();
            if packs.is_empty() {
                usage();
                return ExitCode::FAILURE;
            }
            match cmd_fetch(cfg, &packs).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("错误: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("show") => cmd_show(cfg),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}
