pub mod memory;
pub mod message;
pub mod relation;
pub mod session;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Result, SqlError};

pub struct Database {
    conn: Connection,
}

/// 将 sqlite-vec 注册为 SQLite 自动加载扩展。
fn register_vec_extension() {
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

        init_schema(&conn)?;

        tracing::info!(
            db = %path,
            schema_version = %meta_get(&conn, KEY_SCHEMA_VERSION)?.unwrap_or_default(),
            "database opened"
        );

        Ok(Self { conn })
    }

    pub fn in_memory() -> Result<Self> {
        register_vec_extension();

        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        init_schema(&conn)?;

        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

pub fn ensure_vec_extension(conn: &Connection) -> Result<()> {
    conn.query_row(
        "SELECT vec_version()",
        [],
        |row| row.get::<_, String>(0),
    )
    .map_err(|_| SqlError::Config("sqlite-vec extension not available".to_string()))?;
    Ok(())
}

const SCHEMA_VERSION: i64 = 3;
const KEY_SCHEMA_VERSION: &str = "schema_version";

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;

    migrate(conn)?;

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sessions (
            id         TEXT PRIMARY KEY,
            title      TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
            id         TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role       TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
            content    TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS memories (
            id          TEXT PRIMARY KEY,
            content     TEXT NOT NULL,
            memory_type TEXT NOT NULL DEFAULT 'fact',
            message_id  TEXT REFERENCES messages(id) ON DELETE SET NULL,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entities (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            entity_type TEXT NOT NULL,
            memory_id   TEXT REFERENCES memories(id) ON DELETE CASCADE,
            created_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS relations (
            id            TEXT PRIMARY KEY,
            source_id     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
            target_id     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
            relation_type TEXT NOT NULL,
            weight        REAL DEFAULT 1.0,
            memory_id     TEXT REFERENCES memories(id) ON DELETE CASCADE,
            created_at    TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(memory_type);
        CREATE INDEX IF NOT EXISTS idx_entities_memory ON entities(memory_id);
        CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_id);
        CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_id);
        "#,
    )?;

    let dim = crate::config::Config::get().embedding_dim;
    ensure_vector_table(conn, dim)?;

    ensure_vec_extension(conn)?;
    Ok(())
}

/// 数据库迁移：将旧版（消息即记忆）schema 升级到 v3（会话/消息/事实记忆分离）。
fn migrate(conn: &Connection) -> Result<()> {
    let version = meta_get(conn, KEY_SCHEMA_VERSION)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    // 旧库（无 sessions 表）与新模型差异过大：重建业务表（meta 表保留 embedding_dim）。
    tracing::warn!(
        "数据库 schema 版本 {version} 过旧，重建业务表（memories/entities/relations/vectors 清空）"
    );

    conn.execute_batch(
        "DROP TABLE IF EXISTS relations;
         DROP TABLE IF EXISTS entities;
         DROP TABLE IF EXISTS memory_vectors;
         DROP TABLE IF EXISTS memories;
         DROP TABLE IF EXISTS messages;
         DROP TABLE IF EXISTS sessions;
         DELETE FROM meta WHERE key = 'embedding_dim';",
    )?;

    meta_set(conn, KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    Ok(())
}

const KEY_EMBEDDING_DIM: &str = "embedding_dim";

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query_map([key], |row| row.get::<_, String>(0))?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// 从 sqlite_master 中提取已有 vec0 表创建语句里的维度（兼容无 meta 记录的旧库）。
fn existing_vec_dim(conn: &Connection) -> Result<Option<usize>> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memory_vectors'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else { return Ok(None) };

    const PREFIX: &str = "FLOAT[";
    if let Some(start) = sql.find(PREFIX) {
        let rest = &sql[start + PREFIX.len()..];
        if let Some(end) = rest.find(']') {
            if let Ok(dim) = rest[..end].trim().parse::<usize>() {
                return Ok(Some(dim));
            }
        }
    }
    Ok(None)
}

/// 保证向量表存在且维度与 `dim` 一致。
///
/// 维度不一致时重建 `memory_vectors`：旧向量丢弃（无法跨维度使用），
/// `memories` 文本记录保留，待后续对话重新向量化写入。
fn ensure_vector_table(conn: &Connection, dim: usize) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;

    let stored = meta_get(conn, KEY_EMBEDDING_DIM)?
        .and_then(|v| v.parse::<usize>().ok());
    let actual = existing_vec_dim(conn)?;

    let mismatched = match (stored, actual) {
        (Some(s), _) => s != dim,
        (None, Some(a)) => a != dim,
        (None, None) => false,
    };

    if mismatched {
        let old = actual.or(stored).unwrap_or(0);
        tracing::warn!(
            "embedding 维度 {old} -> {dim}，重建 memory_vectors（旧向量已清除，可重新对话以向量化）",
        );
        conn.execute_batch("DROP TABLE IF EXISTS memory_vectors;")?;
    }

    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors
         USING vec0(memory_id TEXT PRIMARY KEY, embedding FLOAT[{dim}]);"
    ))?;

    meta_set(conn, KEY_EMBEDDING_DIM, &dim.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_with_vec(dim: usize) -> Result<Connection> {
        register_vec_extension();
        let conn = Connection::open_in_memory()?;
        ensure_vector_table(&conn, dim)?;
        Ok(conn)
    }

    #[test]
    fn vector_table_rebuilds_on_dim_change() {
        let conn = open_with_vec(4).unwrap();
        let v = memory::to_byte_array(&[1.0, 2.0, 3.0, 4.0]);
        conn.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('a', ?1)",
            rusqlite::params![v.as_slice()],
        )
        .unwrap();

        // 重建前：数据存在，meta 记 4
        assert_eq!(
            meta_get(&conn, KEY_EMBEDDING_DIM).unwrap().unwrap(),
            "4"
        );

        // 维度变化 → 表重建，旧向量清除
        ensure_vector_table(&conn, 8).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            meta_get(&conn, KEY_EMBEDDING_DIM).unwrap().unwrap(),
            "8"
        );

        // 维度一致 → 数据保留
        let v = memory::to_byte_array(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        conn.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('b', ?1)",
            rusqlite::params![v.as_slice()],
        )
        .unwrap();
        ensure_vector_table(&conn, 8).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn vector_table_dim_detected_from_legacy_db() {
        register_vec_extension();
        let conn = Connection::open_in_memory().unwrap();

        // 模拟旧库：无 meta 记录，但已有 12 维 vec0 表
        conn.execute_batch(
            "CREATE VIRTUAL TABLE memory_vectors
             USING vec0(memory_id TEXT PRIMARY KEY, embedding FLOAT[12]);",
        )
        .unwrap();

        // 维度一致时不重建，数据保留
        let v = memory::to_byte_array(&(0..12).map(|i| i as f32).collect::<Vec<_>>());
        conn.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('legacy', ?1)",
            rusqlite::params![v.as_slice()],
        )
        .unwrap();
        ensure_vector_table(&conn, 12).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "旧库维度一致时不应重建");

        // 与配置不一致 → 重建
        ensure_vector_table(&conn, 20).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "维度不一致应重建并清空旧向量");
    }
}
