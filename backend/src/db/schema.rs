//! 业务表 schema 定义与迁移。
//!
//! 当前 schema 完全由 [`init_schema`] 里的 `CREATE TABLE IF NOT EXISTS` 定义（单一真源），
//! 新库打开时直接建最新结构；历史逐版本迁移（v6~v16）已压平，不再保留。
//! 仅 [`migrate`] 里保留一段 `v17` 清理（删掉三张从未被读写的死表）与末尾的版本戳。
//!
//! 取舍：旧版本库不再支持平滑升级，需重建（`rm` 后重跑）。对单人单库、且已迁移到最新
//! 的项目，这比无限累积迁移代码更划算；早期图谱时代残留的 `entities` 等表本就不在本
//! 文件的建表范围内，亦不处理。

use rusqlite::Connection;

use crate::db::{meta, vec};
use crate::error::Result;

/// 当前 schema 版本号。
pub(crate) const SCHEMA_VERSION: i64 = 17;
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
            created_at TEXT NOT NULL,
            tool_calls TEXT,
            tool_call_id TEXT,
            tool_name TEXT
        );

        CREATE TABLE IF NOT EXISTS memories (
            id          TEXT PRIMARY KEY,
            content     TEXT NOT NULL,
            memory_type TEXT NOT NULL CHECK (memory_type IN ('trait', 'preference', 'routine', 'plan')),
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

        CREATE TABLE IF NOT EXISTS self_profile (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_messages_session_time ON messages(session_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_memories_expires ON memories(expires_at);
        "#,
    )?;

    let cfg = crate::config::Config::get();
    let status = vec::init_vector_table(conn, cfg.embedding_dim, cfg.embedding_model.clone())?;

    vec::ensure_vec_extension(conn)?;

    if let Some(diff) = status.diff_summary() {
        tracing::warn!(
            "记忆向量签名变更：{diff}。程序不会自动重建（memories 的内容/created_at/updated_at 均保留），\
             记忆检索已暂停。确认重建请运行：cargo run --bin sa-mem -- rebuild"
        );
    }
    Ok(())
}

/// 读取已落库的 schema 版本（无记录返回 None）。
pub(crate) fn stored_version(conn: &Connection) -> Result<Option<String>> {
    meta::get(conn, KEY_SCHEMA_VERSION)
}

/// 数据库迁移：**默认只做增量**（新增表/列），不 DROP 业务表。
///
/// memories（content / created_at / updated_at）、messages、sessions 一旦落库即永久保留；
/// 旧版残留的表（如图谱时代的 entities）不删除，只是不再读写。
///
/// 历史逐版本迁移（v6~v16）已压平到 v17 基线：当前 schema 完全由 [`init_schema`] 的
/// `CREATE TABLE IF NOT EXISTS` 定义，新库直接建最新结构，不再需要逐版本补丁。
/// 唯一的 DROP 例外是 v17：删掉三张死表 `relations` / `world_state` / `time_profiles`
/// （代码从不读写，纯 schema 残留，删掉不丢信息）。
fn migrate(conn: &Connection) -> Result<()> {
    let version = meta::get(conn, KEY_SCHEMA_VERSION)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    // 历史逐版本迁移（v6~v16）已压平到 v17 基线：当前 schema 完全由 `init_schema`
    // 的 `CREATE TABLE IF NOT EXISTS` 定义，新库直接建最新结构、无需补丁；真实库
    // 也已在 v17 完成迁移。故此处不再保留逐版本增量迁移，只保留下方 v17 这一段
    // （清理仍可能存在的三张死表）与末尾的版本戳。旧版本库不再支持平滑升级，需重建
    // （`rm` 后重跑）。

    if version < 17 {
        // v17：下线三张死表 `relations` / `world_state` / `time_profiles`。它们由 v7/v8
        // 引入（关系表、世界状态、作息画像），但代码从未读写（无 DAO、无业务调用点），
        // 纯 schema 残留。与 v13 删 `intent_state` 同理——它们存的每个值都不被任何逻辑
        // 当作真值来源，删掉不丢信息。按约定 DROP 业务表需升版本号，故从 16 升到 17。
        // 三张表无任何外键指向或被指向，直接 DROP 即可（无事务/行数校验负担）。
        conn.execute_batch(
            "DROP TABLE IF EXISTS relations;
             DROP TABLE IF EXISTS world_state;
             DROP TABLE IF EXISTS time_profiles;",
        )?;
        tracing::info!("迁移 v17：删除死表 relations / world_state / time_profiles");
    }

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    tracing::info!("数据库 schema 版本 {version} -> {SCHEMA_VERSION}（增量迁移，业务数据保留）");
    meta::set(conn, KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    Ok(())
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

    /// 表是否已存在（sqlite_master）。
    fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// 非破坏式迁移：旧版本库升级时业务表与内容全部保留。
    #[test]
    fn migrate_is_additive_and_preserves_data() {
        let conn = open_conn();
        conn.execute_batch(
            "DROP TABLE IF EXISTS memories;
             CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
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
            .query_row(
                "SELECT content FROM memories WHERE id = 'keep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content, "不要丢我");
    }

    /// v17：下线三张死表 relations / world_state / time_profiles，业务表不受影响。
    #[test]
    fn v17_drops_dead_tables() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             CREATE TABLE relations (channel TEXT NOT NULL, external_id TEXT NOT NULL, closeness REAL NOT NULL DEFAULT 0, trust REAL NOT NULL DEFAULT 0, updated_at TEXT NOT NULL, PRIMARY KEY (channel, external_id));
             CREATE TABLE world_state (id INTEGER PRIMARY KEY CHECK (id = 1), self_base TEXT NOT NULL DEFAULT '', valence REAL NOT NULL DEFAULT 0, energy REAL NOT NULL DEFAULT 0.5, today_date TEXT, today_narrative TEXT NOT NULL DEFAULT '', last_phase TEXT, updated_at TEXT NOT NULL);
             CREATE TABLE time_profiles (channel TEXT NOT NULL, external_id TEXT NOT NULL, utc_offset_minutes INTEGER, active_hour INTEGER, observations INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL, PRIMARY KEY (channel, external_id));
             INSERT INTO messages VALUES ('m1','s1','user','在吗','t');
             INSERT INTO relations VALUES ('telegram','100',0,0,'t');
             INSERT INTO world_state (id, updated_at) VALUES (1,'t');
             INSERT INTO time_profiles (channel, external_id, updated_at) VALUES ('telegram','100','t');
             INSERT INTO meta (key, value) VALUES ('schema_version','16');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            !table_exists(&conn, "relations").unwrap(),
            "relations 应被 v17 删掉"
        );
        assert!(
            !table_exists(&conn, "world_state").unwrap(),
            "world_state 应被 v17 删掉"
        );
        assert!(
            !table_exists(&conn, "time_profiles").unwrap(),
            "time_profiles 应被 v17 删掉"
        );
        // 业务表不受影响。
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "messages 一条不能少");
        assert_eq!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );
    }
}
