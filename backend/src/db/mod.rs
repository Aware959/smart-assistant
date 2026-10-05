pub mod channel;
pub mod memory;
pub mod message;
pub mod self_profile;
pub mod session;

mod meta;
mod schema;
mod vec;

pub use vec::VecStatus;

use vec::{inspect_vec, rebuild_vector_table};

use rusqlite::Connection;

use crate::error::Result;

pub struct Database {
    conn: Connection,
}

/// 将 sqlite-vec 注册为 SQLite 自动加载扩展。
pub(crate) fn register_vec_extension() {
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    }
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        register_vec_extension();

        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        schema::init_schema(&conn)?;

        tracing::info!(
            db = %path,
            schema_version = %schema::stored_version(&conn)?.unwrap_or_default(),
            "database opened"
        );

        Ok(Self { conn })
    }

    pub fn in_memory() -> Result<Self> {
        register_vec_extension();

        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        schema::init_schema(&conn)?;

        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 当前向量库巡检状态（配置签名来自运行时 Config）。
    ///
    /// 会连带 `COUNT(*)` 扫 `memories` 与 `memory_vectors` 两张表——给 `sa-mem`
    /// 这类一次性巡检用。热路径（每轮记忆召回）请改用 [`Self::vec_search_usable`]。
    pub fn vec_status(&self) -> Result<VecStatus> {
        let cfg = crate::config::Config::get();
        inspect_vec(&self.conn, cfg.embedding_dim, &cfg.embedding_model)
    }

    /// 向量检索当前是否可用：**只看维度是否与配置一致**。
    ///
    /// 与 [`VecStatus::search_usable`] 同义，但不构造完整状态、也不做两轮 `COUNT(*)`。
    /// 维度只随 `rebuild_vectors`（DROP + 重建）变化，vec0 的建表语句在
    /// `sqlite_master` 里，改表才会变——因此这里每次都真读也不会有实质开销。
    pub fn vec_search_usable(&self) -> Result<bool> {
        let cfg = crate::config::Config::get();
        Ok(vec::existing_vec_dim(&self.conn)? == Some(cfg.embedding_dim))
    }

    /// 重建向量库并写入新签名；**只影响 memory_vectors**。
    pub fn rebuild_vectors(&self, dim: usize, model: &str) -> Result<()> {
        rebuild_vector_table(&self.conn, dim, model)
    }
}

/// 带诊断的数据库锁获取：记录等待耗时，中毒时回收内部状态而非 panic。
///
/// 全进程共享同一把 `Mutex<Database>`，任何组件持锁时间过长都会让其余组件
/// （telegram 长轮询 / 世界心跳 / 主动引擎）排队——等待超过 1s 就记 warn，
/// 用于暴露"持锁跨 LLM/embedding 阻塞调用"这类隐性卡顿。
pub fn lock_db(db: &std::sync::Mutex<Database>) -> std::sync::MutexGuard<'_, Database> {
    let start = std::time::Instant::now();
    let guard = db.lock().unwrap_or_else(|p| {
        tracing::error!("db 互斥锁中毒：回收内部状态，继续运行");
        p.into_inner()
    });
    let waited = start.elapsed();
    if waited >= std::time::Duration::from_secs(1) {
        tracing::warn!(
            wait_ms = waited.as_millis(),
            "db 锁等待过久（可能有长任务持锁），疑似持锁跨阻塞调用"
        );
    }
    guard
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
