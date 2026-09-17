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
