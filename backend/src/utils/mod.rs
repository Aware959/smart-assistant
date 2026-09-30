//! 与业务无关的通用工具（时间源等）。
//!
//! 统一收走散落在各层的 `Utc::now()` / `Local::now()` 直接调用，保证
//! 本进程内对"现在"的取值一致、可测试。

pub mod runtime;
pub mod textflow;
pub mod time;
pub mod trace;
