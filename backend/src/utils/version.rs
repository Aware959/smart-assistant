//! 自动推进的构建版本号。
//!
//! `Cargo.toml` 只声明 major.minor 基线；patch 位由构建脚本从 Git 提交数
//! 生成，因此每提交一次运行时版本自动加一。短 hash 用于区分同号重建，
//! dirty 标记未提交构建。

/// 应用语义版本，如 `0.2.0`。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 当前 Git 提交数；不在 Git 环境构建时为 `0`。
pub const BUILD_NUMBER: &str = env!("SMART_ASSISTANT_BUILD_NUMBER");

/// 当前 Git 短提交号。
pub const BUILD_COMMIT: &str = env!("SMART_ASSISTANT_BUILD_COMMIT");

/// 构建时工作区是否含未提交改动。
pub const BUILD_DIRTY: &str = env!("SMART_ASSISTANT_BUILD_DIRTY");

/// 语义版本：以 Cargo 的 major.minor 为基线，patch 位自动使用提交数。
pub fn semantic() -> String {
    let mut parts = VERSION.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    let patch = if BUILD_NUMBER == "0" {
        parts.next().unwrap_or("0")
    } else {
        BUILD_NUMBER
    };
    format!("{major}.{minor}.{patch}")
}

/// 展示用版本字符串。
pub fn display() -> String {
    let dirty = if BUILD_DIRTY == "1" { ".dirty" } else { "" };
    format!("Zhiwei v{}+{BUILD_COMMIT}{dirty}", semantic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_cargo_package_version() {
        assert!(display().starts_with("Zhiwei v0.2."));
        assert!(!BUILD_NUMBER.trim().is_empty());
        assert!(!BUILD_COMMIT.trim().is_empty());
        assert!(semantic().split('.').count() == 3);
        assert!(!VERSION.trim().is_empty());
    }
}
