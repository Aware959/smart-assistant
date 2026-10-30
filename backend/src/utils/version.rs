//! 构建版本号的统一入口。
//!
//! 版本来自 `Cargo.toml` 的 package 版本，编译期写入；运行日志、`--version`
//! 和 Telegram `/version` 共用同一份，避免每次排查再人工确认。

/// 应用语义版本，如 `0.2.0`。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 展示用版本字符串。
pub fn display() -> String {
    format!("smart-assistant v{VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_cargo_package_version() {
        assert_eq!(display(), format!("smart-assistant v{VERSION}"));
        assert!(!VERSION.trim().is_empty());
    }
}
