//! 业务表 schema 定义与增量迁移。
//!
//! 迁移**只做增量**（新增表/列），绝不 DROP 业务表：memories / messages / sessions
//! 一旦落库即永久保留；旧版残留的表（如图谱时代的 entities/relations）不删除，只是不再读写。

use rusqlite::Connection;

use crate::db::{meta, vec};
use crate::error::Result;

/// 当前 schema 版本号。
pub(crate) const SCHEMA_VERSION: i64 = 10;
const KEY_SCHEMA_VERSION: &str = "schema_version";

/// 建表（幂等）+ 增量迁移 + 向量表初始化。数据库打开时调用。
pub(crate) fn init_schema(conn: &Connection) -> Result<()> {
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

        CREATE TABLE IF NOT EXISTS intent_state (
            channel            TEXT NOT NULL,
            external_id        TEXT NOT NULL,
            session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            last_user_reply_at TEXT,
            last_intent_at     TEXT,
            today_count        INTEGER NOT NULL DEFAULT 0,
            today_date         TEXT,
            PRIMARY KEY (channel, external_id)
        );

        CREATE TABLE IF NOT EXISTS time_profiles (
            channel            TEXT NOT NULL,
            external_id        TEXT NOT NULL,
            utc_offset_minutes INTEGER,
            active_hour        INTEGER,
            observations       INTEGER NOT NULL DEFAULT 0,
            updated_at         TEXT NOT NULL,
            PRIMARY KEY (channel, external_id)
        );

        CREATE TABLE IF NOT EXISTS self_profile (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS world_state (
            id              INTEGER PRIMARY KEY CHECK (id = 1),
            self_base       TEXT NOT NULL DEFAULT '',
            valence         REAL NOT NULL DEFAULT 0,
            energy          REAL NOT NULL DEFAULT 0.5,
            today_date      TEXT,
            today_narrative TEXT NOT NULL DEFAULT '',
            last_phase      TEXT,
            updated_at      TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS relations (
            channel        TEXT NOT NULL,
            external_id    TEXT NOT NULL,
            closeness      REAL NOT NULL DEFAULT 0,
            trust          REAL NOT NULL DEFAULT 0,
            updated_at     TEXT NOT NULL,
            PRIMARY KEY (channel, external_id)
        );

        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(memory_type);
        "#,
    )?;

    let cfg = crate::config::Config::get();
    let status = vec::init_vector_table(conn, cfg.embedding_dim, cfg.embedding_model.clone())?;

    vec::ensure_vec_extension(conn)?;

    if let Some(diff) = status.diff_summary() {
        tracing::warn!(
            "记忆向量签名变更：{diff}。程序不会自动重建（memories 的内容/created_at/updated_at 均保留），\
             记忆检索已暂停。确认重建请运行：cargo run --bin smart-assistant-memory -- rebuild"
        );
    }
    Ok(())
}

/// 读取已落库的 schema 版本（无记录返回 None）。
pub(crate) fn stored_version(conn: &Connection) -> Result<Option<String>> {
    meta::get(conn, KEY_SCHEMA_VERSION)
}

/// 数据库迁移：**只做增量**（新增表/列），绝不 DROP 业务表。
///
/// memories（content / created_at / updated_at）、messages、sessions 一旦落库即永久保留；
/// 旧版残留的表（如图谱时代的 entities/relations）不删除，只是不再读写。
fn migrate(conn: &Connection) -> Result<()> {
    let version = meta::get(conn, KEY_SCHEMA_VERSION)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    if version < 6 {
        // v6：记忆分层（tier 短/中/长 + 过期时间）。旧库补列，新库建表已带。
        add_column_if_missing(conn, "memories", "tier", "TEXT NOT NULL DEFAULT 'core'")?;
        add_column_if_missing(conn, "memories", "expires_at", "TEXT")?;
    }

    if version < 7 {
        // v7：时间世界模型画像表（对方作息/推断时区；仅新增，不动旧表）。
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS time_profiles (
                channel            TEXT NOT NULL,
                external_id        TEXT NOT NULL,
                utc_offset_minutes INTEGER,
                active_hour        INTEGER,
                observations       INTEGER NOT NULL DEFAULT 0,
                updated_at         TEXT NOT NULL,
                PRIMARY KEY (channel, external_id)
            );
            "#,
        )?;
    }

    if version < 8 {
        // v8：世界引擎 —— AI 自身状态表（情绪/今日叙事/自我档案）与关系表（按用户）。
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS world_state (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                self_base       TEXT NOT NULL DEFAULT '',
                valence         REAL NOT NULL DEFAULT 0,
                energy          REAL NOT NULL DEFAULT 0.5,
                today_date      TEXT,
                today_narrative TEXT NOT NULL DEFAULT '',
                last_phase      TEXT,
                updated_at      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS relations (
                channel        TEXT NOT NULL,
                external_id    TEXT NOT NULL,
                closeness      REAL NOT NULL DEFAULT 0,
                trust          REAL NOT NULL DEFAULT 0,
                updated_at     TEXT NOT NULL,
                PRIMARY KEY (channel, external_id)
            );
            "#,
        )?;
    }

    if version < 9 {
        // v9：命名统一 proactive → intent（表名 + 列名）。旧表有数据则整体改名
        // （SQLite RENAME 保留数据与外键），新库由 init_schema 的 CREATE TABLE 建好。
        if table_exists(conn, "proactive_state")? {
            conn.execute_batch(
                "ALTER TABLE proactive_state RENAME TO intent_state;",
            )?;
            if !column_exists(conn, "intent_state", "last_intent_at")? {
                conn.execute_batch(
                    "ALTER TABLE intent_state RENAME COLUMN last_proactive_at TO last_intent_at;",
                )?;
            }
        }
    }

    if version < 10 {
        // v10：AI 自我属性 KV 表（性格/三观/爱好…），人设数据源，由 core/self_profile 读写。
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS self_profile (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )?;
    }

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    tracing::info!("数据库 schema 版本 {version} -> {SCHEMA_VERSION}（增量迁移，业务数据保留）");
    meta::set(conn, KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    Ok(())
}

/// 给旧表补列（表不存在或列已存在则跳过）。SQLite 的 ALTER 无法加带约束列到
/// 尚未创建的表，新库的列由 init_schema 的 CREATE TABLE 负责，这里只服务旧库。
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, def: &str) -> Result<()> {
    if !table_exists(conn, table)? {
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

/// 表是否已存在（sqlite_master）。
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// 表是否存在指定列（PRAGMA table_info）。
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let table_exists = table_exists(conn, table)?;
    if !table_exists {
        return Ok(false);
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols.iter().any(|c| c == column))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn
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
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );

        let content: String = conn
            .query_row("SELECT content FROM memories WHERE id = 'keep'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(content, "不要丢我");
    }

    /// v9：旧库的 proactive_state 整体改名为 intent_state、列 last_proactive_at 改名
    /// last_intent_at，业务数据保留。
    #[test]
    fn v9_renames_proactive_table_to_intent() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE proactive_state (
                channel            TEXT NOT NULL,
                external_id        TEXT NOT NULL,
                session_id         TEXT NOT NULL,
                last_user_reply_at TEXT,
                last_proactive_at  TEXT,
                today_count        INTEGER NOT NULL DEFAULT 0,
                today_date         TEXT,
                PRIMARY KEY (channel, external_id)
            );
            INSERT INTO proactive_state (channel, external_id, session_id, last_proactive_at)
            VALUES ('telegram', '100', 's1', '2026-01-01T00:00:00Z');
            INSERT INTO meta (key, value) VALUES ('schema_version', '8');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(!table_exists(&conn, "proactive_state").unwrap());
        assert!(table_exists(&conn, "intent_state").unwrap());
        assert!(column_exists(&conn, "intent_state", "last_intent_at").unwrap());
        let ts: String = conn
            .query_row(
                "SELECT last_intent_at FROM intent_state WHERE channel='telegram' AND external_id='100'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ts, "2026-01-01T00:00:00Z");
    }

    /// v10：旧库（v9 无 self_profile 表）增量迁移后建出该表，业务数据不受影响。
    #[test]
    fn v10_creates_self_profile_table() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             INSERT INTO messages (id, session_id, role, content, created_at)
             VALUES ('keep', 's1', 'user', '别丢', 't');
             INSERT INTO meta (key, value) VALUES ('schema_version', '9');",
        )
        .unwrap();
        assert!(!table_exists(&conn, "self_profile").unwrap());

        migrate(&conn).unwrap();

        assert!(table_exists(&conn, "self_profile").unwrap());
        assert!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap()
                == SCHEMA_VERSION.to_string()
        );
        let content: String = conn
            .query_row("SELECT content FROM messages WHERE id = 'keep'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(content, "别丢");
    }
}
