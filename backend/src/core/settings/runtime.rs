//! 进程内运行时配置快照：写入成功后原子替换，读取方每次取一份 clone。

use std::sync::RwLock;

use crate::core::settings::contract::RuntimeSettings;

#[derive(Debug)]
pub struct SettingsRuntime {
    current: RwLock<RuntimeSettings>,
    revision: RwLock<i64>,
}

impl SettingsRuntime {
    pub fn new(current: RuntimeSettings, revision: i64) -> Self {
        Self {
            current: RwLock::new(current),
            revision: RwLock::new(revision),
        }
    }

    pub fn snapshot(&self) -> RuntimeSettings {
        self.current
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn revision(&self) -> i64 {
        *self
            .revision
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn replace(&self, settings: RuntimeSettings, revision: i64) {
        *self
            .current
            .write()
            .unwrap_or_else(|error| error.into_inner()) = settings;
        *self
            .revision
            .write()
            .unwrap_or_else(|error| error.into_inner()) = revision;
    }
}
