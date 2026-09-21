//! 统一的"现在"时间源。
//!
//! 世界心跳、作息画像增量、叙事断言、主动决策都从这里取"现在"，
//! 便于将来整体替换（回放 / 测试时钟），而不是散落 `Utc::now()` / `Local::now()`。

use chrono::{DateTime, Local, Timelike, Utc};

/// 世界行进用的"现在"（UTC）。
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// "现在"的本地钟表日期（yyyy-mm-dd）：叙事跨天判断、主动配额日界。
pub fn local_date() -> String {
    Local::now().date_naive().to_string()
}

/// "现在"的本地钟表小时（0-23）：活跃阶段映射。
pub fn local_hour() -> u32 {
    Local::now().hour()
}