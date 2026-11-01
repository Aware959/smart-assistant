//! 共享运行时配置域。
//!
//! 契约与校验不依赖桌面或传输层；当前由 `zhiwei-service` 作为 runtime host 托管，
//! 后续 Linux / Android 可复用同一 service。

pub mod contract;
pub mod runtime;
pub mod service;

use std::sync::{Arc, OnceLock};

pub use contract::{RuntimeSettings, SettingsKey, SettingsSchemaItem};
pub use runtime::SettingsRuntime;
pub use service::{SettingsAuditEntry, SettingsService, SettingsUpdate, SettingsView};

static PROCESS_RUNTIME: OnceLock<Arc<SettingsRuntime>> = OnceLock::new();
static BOOTSTRAP_RUNTIME: OnceLock<Arc<SettingsRuntime>> = OnceLock::new();

/// 安装进程级运行时配置。只有首次装配生效；后续 App（测试）不会覆盖。
pub fn install_runtime(runtime: Arc<SettingsRuntime>) {
    let _ = PROCESS_RUNTIME.set(runtime);
}

/// 当前进程配置快照。尚未安装时回退到 `.env`，供纯函数测试使用。
pub fn runtime() -> Arc<SettingsRuntime> {
    PROCESS_RUNTIME.get().cloned().unwrap_or_else(|| {
        BOOTSTRAP_RUNTIME
            .get_or_init(|| {
                let bootstrap = RuntimeSettings::from_bootstrap(crate::config::Config::get());
                Arc::new(SettingsRuntime::new(bootstrap, 0))
            })
            .clone()
    })
}

pub fn snapshot() -> RuntimeSettings {
    runtime().snapshot()
}
