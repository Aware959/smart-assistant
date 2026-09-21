//! 层守卫：把"分层规则"固化成可执行的测试。
//!
//! 规则来源见 `AGENTS.md`：
//! - **core**：应用本体（会话 / 记忆 / 主动 / 运行时 / 组装层），依赖 db 与 llm，
//!   能力以具体类型直接接线；不得反向依赖传输层（`channels` / `api`）——唯一例外是
//!   主动外发的具体实现 [`ChannelsPusher`]（core 需要它是"外部能力"而非传输端点）；
//! - **db 层**：只做 SQL，不向业务暴露公共 API，表所有权归对应领域适配器；
//! - **传输层（api / channels）**：直接持主线聊天服务（[`crate::core::agent::Agent`]）
//!   及各服务引用走方法调用，但禁止 import `db::*`——数据一律经服务方法读写，db 不出壳。

use std::path::{Path, PathBuf};

/// 应用本体（core 允许直接 import 的能力实现，属于"外部能力"而非传输端点）。
const CORE_ALLOWED_CAPABILITIES: &[&str] = &["use crate::channels::ChannelsPusher"];

/// 应用本体：不得出现对传输层的引用。
const CORE_LAYERS: &[&str] = &["src/core"];

/// 被禁碰 db / 组合根的目录（传输层）。
const TRANSPORT_LAYERS: &[&str] = &["src/api", "src/channels"];

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                files.extend(walk(&p));
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                files.push(p);
            }
        }
    }
    files
}

fn lines(f: &Path) -> Vec<(usize, String)> {
    std::fs::read_to_string(f)
        .unwrap_or_else(|e| panic!("读取 {} 失败：{e}", f.display()))
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.to_string()))
        .collect()
}

/// core（应用本体）不得 import 传输层：依赖方向固定为「传输层 → core」，
/// 反向引用会把 HTTP / 通道耦合进内核，破坏边界。
/// 唯一例外：主动外发的具体能力 [`ChannelsPusher`]（见 `CORE_ALLOWED_CAPABILITIES`）。
#[test]
fn core_never_imports_transport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    for rel in CORE_LAYERS {
        for f in walk(&root.join(rel)) {
            for (no, line) in lines(&f) {
                let c = line.trim_start();
                let code = c.trim_end_matches(';');
                if CORE_ALLOWED_CAPABILITIES.contains(&code) {
                    continue;
                }
                if c.starts_with("use crate::channels")
                    || c.starts_with("use crate::api")
                    || c.starts_with("use smart_assistant::channels")
                    || c.starts_with("use smart_assistant::api")
                {
                    violations.push(format!(
                        "{}:{no}  {}",
                        f.strip_prefix(root).unwrap().display(),
                        c
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "core 不得反向依赖传输层（channels/api），请改走具体能力（唯一例外：channels::ChannelsPusher）：\n{}",
        violations.join("\n")
    );
}

/// 传输层不得出现任何对 db 的引用（`use crate::db` / `crate::db::`）。
/// 直连会让 SQL 重新变成公共 API，绕过「组合根 → 适配器」的能力收口。
#[test]
fn transport_layers_never_import_db() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    for rel in TRANSPORT_LAYERS {
        let p = root.join(rel);
        let files = if p.is_dir() { walk(&p) } else { vec![p] };
        for f in files {
            for (no, line) in lines(&f) {
                let code = line.trim_start();
                if code.starts_with("use crate::db")
                    || code.starts_with("use smart_assistant::db")
                    || code.contains("crate::db::")
                {
                    violations.push(format!(
                        "{}:{no}  {}",
                        f.strip_prefix(root).unwrap().display(),
                        code
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "传输层（api/channels）不得触碰 db，请改走服务方法：\n{}",
        violations.join("\n")
    );
}

#[test]
fn sanity_of_guard_walker() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let core_has_files = walk(&root.join("src/core")).len() > 5;
    assert!(core_has_files, "守卫应至少能枚举到 core 的源文件");
}