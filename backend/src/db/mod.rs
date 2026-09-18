pub mod channel;
pub mod memory;
pub mod message;
pub mod proactive;
pub mod relations;
pub mod session;
pub mod timeworld;
pub mod world_state;

mod meta;
mod schema;
mod vec;

pub use vec::{ensure_vec_extension, inspect_vec, rebuild_vector_table, VecStatus};

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
        >(sqlite_vec::sqlite3_vec_init as *const ())));
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
    pub fn vec_status(&self) -> Result<VecStatus> {
        let cfg = crate::config::Config::get();
        inspect_vec(&self.conn, cfg.embedding_dim, &cfg.embedding_model)
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
pub fn lock_db(
    db: &std::sync::Mutex<Database>,
) -> std::sync::MutexGuard<'_, Database> {
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
