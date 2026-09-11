pub mod channel;
pub mod memory;
pub mod message;
pub mod proactive;
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

pub fn ensure_vec_extension(conn: &Connection) -> Result<()> {
    conn.query_row(
        "SELECT vec_version()",
        [],
        |row| row.get::<_, String>(0),
    )
    .map_err(|_| SqlError::Config("sqlite-vec extension not available".to_string()))?;
    Ok(())
}

const SCHEMA_VERSION: i64 = 6;
const KEY_SCHEMA_VERSION: &str = "schema_version";

/// 向量表的“签名”：embedding 维度 + 模型名。
/// 签名与运行时配置不一致时，程序只提醒、绝不自动重建；
/// 由独立工具 `smart-assistant-memory rebuild`（或 --force）处理。
const KEY_EMBEDDING_DIM: &str = "embedding_dim";
const KEY_EMBEDDING_MODEL: &str = "embedding_model";

/// 记忆向量库状态（只读巡检结果）。
#[derive(Debug, Clone)]
pub struct VecStatus {
    pub configured_dim: usize,
    pub configured_model: String,
    /// meta 中记录的维度（首次建表时写入；旧库可能为 None）。
    pub stored_dim: Option<usize>,
    /// meta 中记录的模型（首次建表时写入；旧库可能为 None）。
    pub stored_model: Option<String>,
    /// sqlite_master 里向量表实际定义的维度；表不存在时为 None。
    pub actual_dim: Option<usize>,
    pub memory_count: i64,
    pub vector_count: i64,
}

impl VecStatus {
    /// 是否需要重建向量库（维度或模型签名与配置不一致，且该变更可被察觉）。
    pub fn needs_rebuild(&self) -> bool {
        self.actual_dim != Some(self.configured_dim)
            || self
                .stored_model
                .as_deref()
                .map(|m| m != self.configured_model)
                .unwrap_or(false)
    }

    /// 向量检索当前是否可用（仅维度一致即可 MATCH；模型差异只影响质量，不阻塞）。
    pub fn search_usable(&self) -> bool {
        self.actual_dim == Some(self.configured_dim)
    }

    /// 人类可读的签名差异描述（无差异返回 None）。
    pub fn diff_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        let actual = self.actual_dim;
        if actual != Some(self.configured_dim) {
            let s = actual.map(|d| d.to_string()).unwrap_or_else(|| "缺失".into());
            parts.push(format!("embedding_dim {s} -> {}", self.configured_dim));
        }
        if let Some(m) = self.stored_model.as_deref() {
            if m != self.configured_model {
                parts.push(format!("embedding_model {m} -> {}", self.configured_model));
            }
        } else if actual.is_none() {
            parts.push(format!(
                "向量表缺失，将按 embedding_dim {} / {} 创建",
                self.configured_dim, self.configured_model
            ));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

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
            tier        TEXT NOT NULL DEFAULT 'core',
            expires_at  TEXT,
            message_id  TEXT REFERENCES messages(id) ON DELETE SET NULL,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS channel_sessions (
            channel     TEXT NOT NULL,
            external_id TEXT NOT NULL,
            session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            PRIMARY KEY (channel, external_id)
        );

        CREATE TABLE IF NOT EXISTS proactive_state (
            channel            TEXT NOT NULL,
            external_id        TEXT NOT NULL,
            session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            last_user_reply_at TEXT,
            last_proactive_at  TEXT,
            today_count        INTEGER NOT NULL DEFAULT 0,
            today_date         TEXT,
            PRIMARY KEY (channel, external_id)
        );

        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(memory_type);
        "#,
    )?;

    let cfg = crate::config::Config::get();
    let status = init_vector_table(conn, cfg.embedding_dim, cfg.embedding_model.clone())?;

    ensure_vec_extension(conn)?;

    if let Some(diff) = status.diff_summary() {
        tracing::warn!(
            "记忆向量签名变更：{diff}。程序不会自动重建（memories 的内容/created_at/updated_at 均保留），\
             记忆检索已暂停。确认重建请运行：cargo run --bin smart-assistant-memory -- rebuild"
        );
    }
    Ok(())
}

/// 数据库迁移：**只做增量**（新增表/列），绝不 DROP 业务表。
///
/// memories（content / created_at / updated_at）、messages、sessions 一旦落库即永久保留；
/// 旧版残留的表（如图谱时代的 entities/relations）不删除，只是不再读写。
fn migrate(conn: &Connection) -> Result<()> {
    let version = meta_get(conn, KEY_SCHEMA_VERSION)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    if version < 6 {
        // v6：记忆分层（tier 短/中/长 + 过期时间）。旧库补列，新库建表已带。
        add_column_if_missing(conn, "memories", "tier", "TEXT NOT NULL DEFAULT 'core'")?;
        add_column_if_missing(conn, "memories", "expires_at", "TEXT")?;
    }

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    tracing::info!("数据库 schema 版本 {version} -> {SCHEMA_VERSION}（增量迁移，业务数据保留）");
    meta_set(conn, KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    Ok(())
}

/// 给旧表补列（表不存在或列已存在则跳过）。SQLite 的 ALTER 无法加带约束列到
/// 尚未创建的表，新库的列由 init_schema 的 CREATE TABLE 负责，这里只服务旧库。
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, def: &str) -> Result<()> {
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(());
    }

    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if cols.iter().any(|c| c == column) {
        return Ok(());
    }

    conn.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {def};"
    ))?;
    tracing::info!("迁移：memories 补列 {column} {def}");
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

/// 只读巡检向量库状态，不建表、不改任何数据。
pub fn inspect_vec(
    conn: &Connection,
    configured_dim: usize,
    configured_model: &str,
) -> Result<VecStatus> {
    let stored_dim = meta_get(conn, KEY_EMBEDDING_DIM)?
        .and_then(|v| v.parse::<usize>().ok());
    let stored_model = meta_get(conn, KEY_EMBEDDING_MODEL)?;
    let actual_dim = existing_vec_dim(conn)?;

    let memory_count = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    let vector_count = match actual_dim {
        Some(_) => conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))?,
        None => 0,
    };

    Ok(VecStatus {
        configured_dim,
        configured_model: configured_model.to_string(),
        stored_dim,
        stored_model,
        actual_dim,
        memory_count,
        vector_count,
    })
}

/// 保证向量表在“首次（或表被误删）时”存在，并记录签名；**签名不一致绝不重建**。
///
/// - 表不存在且 meta 有签名 → 按 meta 记录的维度恢复空表（不改变签名）；
/// - 表不存在且 meta 无签名（全新库）→ 按配置维度创建并写入签名；
/// - 表已存在 → 原样保留，差异仅体现在返回的 [`VecStatus`] 中。
fn init_vector_table(conn: &Connection, configured_dim: usize, configured_model: String) -> Result<VecStatus> {
    let stored_dim = meta_get(conn, KEY_EMBEDDING_DIM)?
        .and_then(|v| v.parse::<usize>().ok());

    if existing_vec_dim(conn)?.is_none() {
        let dim = stored_dim.unwrap_or(configured_dim);
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors
             USING vec0(memory_id TEXT PRIMARY KEY, embedding FLOAT[{dim}]);"
        ))?;
        // 只有全新库才写签名；表被误删时保持原有签名让用户自行 decision。
        if stored_dim.is_none() {
            meta_set(conn, KEY_EMBEDDING_DIM, &configured_dim.to_string())?;
            meta_set(conn, KEY_EMBEDDING_MODEL, &configured_model)?;
        }
    }

    inspect_vec(conn, configured_dim, &configured_model)
}

/// 重建向量库：**只**动 memory_vectors（DROP + 按新签名重建），memories 完全不动。
pub fn rebuild_vector_table(conn: &Connection, dim: usize, model: &str) -> Result<()> {
    conn.execute_batch("DROP TABLE IF EXISTS memory_vectors;")?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE memory_vectors
         USING vec0(memory_id TEXT PRIMARY KEY, embedding FLOAT[{dim}]);"
    ))?;
    meta_set(conn, KEY_EMBEDDING_DIM, &dim.to_string())?;
    meta_set(conn, KEY_EMBEDDING_MODEL, model)?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        register_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS memories (
                 id          TEXT PRIMARY KEY,
                 content     TEXT NOT NULL,
                 memory_type TEXT NOT NULL DEFAULT 'fact',
                 message_id  TEXT,
                 created_at  TEXT NOT NULL,
                 updated_at  TEXT NOT NULL
             );",
        )
        .unwrap();
        conn
    }

    fn with_memories(conn: &Connection, ids: &[&str]) {
        for id in ids {
            conn.execute(
                "INSERT INTO memories (id, content, memory_type, message_id, created_at, updated_at)
                 VALUES (?1, 'content-' || ?1, 'fact', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [id],
            )
            .unwrap();
        }
    }

    /// 全新建空表：无 meta、无表 → 按配置维度创建并写入签名。
    #[test]
    fn fresh_db_creates_vector_table_with_signature() {
        let conn = open_conn();
        with_memories(&conn, &["a", "b"]);
        let st = init_vector_table(&conn, 4, "model-x".to_string()).unwrap();
        assert_eq!(st.actual_dim, Some(4));
        assert_eq!(st.stored_dim, Some(4));
        assert_eq!(st.stored_model.as_deref(), Some("model-x"));
        assert!(!st.needs_rebuild());
        assert!(st.search_usable());
        assert_eq!(st.memory_count, 2);
        assert_eq!(st.vector_count, 0);
    }

    /// 签名不一致：仅报告，绝不 DROP，维度、既有向量与 meta 全部保留。
    #[test]
    fn dim_change_is_reported_but_never_rebuilt() {
        let conn = open_conn();
        init_vector_table(&conn, 4, "model-x".to_string()).unwrap();
        let v4 = memory::to_byte_array(&[1.0, 2.0, 3.0, 4.0]);
        conn.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('a', ?1)",
            rusqlite::params![v4.as_slice()],
        )
        .unwrap();

        let st = init_vector_table(&conn, 8, "model-x".to_string()).unwrap();
        assert_eq!(st.actual_dim, Some(4), "表不应被重建");
        assert_eq!(st.stored_dim, Some(4), "meta 不应被改写");
        assert!(st.needs_rebuild());
        assert!(!st.search_usable());

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "旧向量应原样保留");
        assert_eq!(meta_get(&conn, KEY_EMBEDDING_DIM).unwrap().unwrap(), "4");
    }

    /// 模型签名变化（维度不变）同样被识别为需要重建，但检索仍可用。
    #[test]
    fn model_change_is_detected() {
        let conn = open_conn();
        init_vector_table(&conn, 4, "model-x".to_string()).unwrap();
        let st = init_vector_table(&conn, 4, "model-y".to_string()).unwrap();
        assert!(st.needs_rebuild());
        assert!(st.search_usable(), "维度一致时 MATCH 仍可用");
        assert_eq!(
            st.diff_summary().unwrap(),
            "embedding_model model-x -> model-y"
        );
    }

    /// 表被误删但有签名：按 meta 维度恢复，不改变签名；与配置的差异照常提示重建。
    #[test]
    fn dropped_table_recovers_to_stored_signature() {
        let conn = open_conn();
        init_vector_table(&conn, 4, "model-x".to_string()).unwrap();
        conn.execute_batch("DROP TABLE memory_vectors;").unwrap();

        // 配置是 8 维，但 meta 记录 4 维 → 恢复成 4 维以保持签名一致，
        // 同时与配置的差异仍需提示重建（config 8 != actual 4）。
        let st = init_vector_table(&conn, 8, "model-x".to_string()).unwrap();
        assert_eq!(st.actual_dim, Some(4));
        assert!(st.needs_rebuild());
        assert_eq!(st.stored_dim, Some(4));
        assert_eq!(
            meta_get(&conn, KEY_EMBEDDING_MODEL).unwrap().unwrap(),
            "model-x"
        );

        // 配置恰好等于恢复维度时，则一切正常
        let st = init_vector_table(&conn, 4, "model-x".to_string()).unwrap();
        assert!(!st.needs_rebuild());
        assert!(st.search_usable());
    }

    /// 旧库（无 meta、表定义 12 维）：识别维度，一致时可用。
    #[test]
    fn legacy_db_dim_detected_without_meta() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE memory_vectors
             USING vec0(memory_id TEXT PRIMARY KEY, embedding FLOAT[12]);",
        )
        .unwrap();
        let v = memory::to_byte_array(&(0..12).map(|i| i as f32).collect::<Vec<_>>());
        conn.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('legacy', ?1)",
            rusqlite::params![v.as_slice()],
        )
        .unwrap();

        let st = init_vector_table(&conn, 12, "model-x".to_string()).unwrap();
        assert_eq!(st.stored_dim, None, "旧库不写 meta");
        assert!(!st.needs_rebuild());
        assert!(st.search_usable());

        // 换成 20 维配置：报告差异，保留旧表
        let st = init_vector_table(&conn, 20, "model-x".to_string()).unwrap();
        assert!(st.needs_rebuild());
        assert!(!st.search_usable());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "旧向量保留");
    }

    /// rebuild_vector_table 只重建向量表并更新签名，memories 内容不动。
    #[test]
    fn rebuild_keeps_memories_intact() {
        let conn = open_conn();
        init_vector_table(&conn, 4, "model-x".to_string()).unwrap();
        with_memories(&conn, &["a", "b"]);
        conn.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES ('a', ?1)",
            rusqlite::params![memory::to_byte_array(&[1.0, 2.0, 3.0, 4.0]).as_slice()],
        )
        .unwrap();

        rebuild_vector_table(&conn, 8, "model-y").unwrap();

        let st = inspect_vec(&conn, 8, "model-y").unwrap();
        assert_eq!(st.actual_dim, Some(8));
        assert!(st.search_usable());
        assert_eq!(st.vector_count, 0);
        assert_eq!(st.memory_count, 2, "memories 不应受影响");

        let content: String = conn
            .query_row("SELECT content FROM memories WHERE id = 'a'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(content, "content-a");
    }

    /// 非破坏式迁移：旧版本库升级时业务表与内容全部保留。
    #[test]
    fn migrate_is_additive_and_preserves_data() {
        let conn = open_conn();
        conn.execute_batch(
            "DROP TABLE IF EXISTS memories;
             CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, memory_type TEXT, message_id TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO memories (id, content, memory_type, message_id, created_at, updated_at)
             VALUES ('keep', '不要丢我', 'fact', NULL, 't', 't');",
        )
        .unwrap();

        migrate(&conn).unwrap();
        assert_eq!(
            meta_get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );

        let content: String = conn
            .query_row("SELECT content FROM memories WHERE id = 'keep'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(content, "不要丢我");
    }
}
