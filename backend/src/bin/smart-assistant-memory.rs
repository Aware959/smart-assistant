/// 记忆向量库维护工具：与主程序（server / cli）完全解耦。
///
/// 主程序检测到向量签名变更（embedding_dim / embedding_model）时**不会**自动重建，
/// 只会提醒；是否需要重建由该工具决定并执行。
///
/// 用法：
///   smart-assistant-memory check                巡检签名与现状（只读）
///   smart-assistant-memory rebuild [--force]    重建 memory_vectors 并按 memories.content 重新向量化
///
/// 安全性保证：
///   - 只操作 memory_vectors 表；memories（content/created_at/updated_at）永不改动；
///   - 签名一致且未加 --force 时拒绝 rebuild；
///   - 建议先停止 server 再执行 rebuild。
use std::process::ExitCode;

use smart_assistant::config::Config;
use smart_assistant::db;
use smart_assistant::memory;

fn banner(title: &str) {
    println!("\n===== smart-assistant-memory: {title} =====");
}

fn print_status(status: &db::VecStatus) {
    println!("  配置签名      : embedding_dim = {} , embedding_model = {}",
        status.configured_dim, status.configured_model);
    println!("  存储签名      : embedding_dim = {} , embedding_model = {}",
        status.stored_dim.map(|d| d.to_string()).unwrap_or_else(|| "未记录".into()),
        status.stored_model.as_deref().unwrap_or("未记录"));
    println!("  向量表实际维度: {}", status.actual_dim.map(|d| d.to_string()).unwrap_or_else(|| "表不存在".into()));
    println!("  memories 记录 : {} 条", status.memory_count);
    println!("  memory_vectors: {} 条", status.vector_count);
    if status.needs_rebuild() {
        println!("  状态          : 签名不一致，需要重建（{}）", status.diff_summary().unwrap_or_default());
    } else if status.search_usable() {
        println!("  状态          : 正常，记忆检索可用");
    } else {
        println!("  状态          : 表缺失且无签名（全新库由主程序创建）");
    }
}

fn cmd_check(cfg: &Config) -> Result<(), String> {
    banner("check");
    let db = db::Database::open(&cfg.db_path).map_err(|e| e.to_string())?;
    print_status(&db.vec_status().map_err(|e| e.to_string())?);
    Ok(())
}

fn cmd_rebuild(cfg: &Config, force: bool) -> Result<(), String> {
    banner("rebuild");
    let db = db::Database::open(&cfg.db_path).map_err(|e| e.to_string())?;
    let status = db.vec_status().map_err(|e| e.to_string())?;
    print_status(&status);

    if !status.needs_rebuild() && !force {
        println!("\n  签名一致，无需重建。仍要全部重新向量化请加 --force。");
        println!("  已放弃，未做任何修改。");
        return Ok(());
    }
    if status.needs_rebuild() {
        println!("\n  确认重建：仅重建 memory_vectors，memories(content/created_at/updated_at) 不动。");
    } else {
        println!("\n  强制重建（--force）：仅重建 memory_vectors，memories 不动。");
    }

    println!("  开始按 memories.content 重新向量化...\n");
    let done = memory::store::rebuild_all(&db).map_err(|e| e.to_string())?;

    let after = db.vec_status().map_err(|e| e.to_string())?;
    println!("  重建完成：成功写入 {done} 条向量。");
    print_status(&after);
    println!("  memories 内容（content/created_at/updated_at）未做任何改动。");
    Ok(())
}

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let cfg = Config::get();

    match args.first().map(String::as_str) {
        Some("check") => match cmd_check(&cfg) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("错误: {e}");
                ExitCode::FAILURE
            }
        },
        Some("rebuild") => {
            let force = args.iter().any(|a| a == "--force");
            match cmd_rebuild(&cfg, force) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("错误: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            println!("用法:\n  smart-assistant-memory check          巡检签名与现状（只读）\n  smart-assistant-memory rebuild [--force]  重建 memory_vectors（建议先停 server）");
            ExitCode::FAILURE
        }
    }
}