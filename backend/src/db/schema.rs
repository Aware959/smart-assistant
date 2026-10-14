//! 业务表 schema 定义与迁移。
//!
//! 当前 schema 完全由 [`init_schema`] 里的 `CREATE TABLE IF NOT EXISTS` 定义（单一真源），
//! 新库打开时直接建最新结构；历史逐版本迁移（v6~v16）已压平，不再保留。
//! 仅 [`migrate`] 里保留 `v17`（删掉三张从未被读写的死表）、`v18`（`messages` 放开
//! `role='tool'` 并改写历史工具结果行）两段，与末尾的版本戳。
//!
//! 取舍：旧版本库不再支持平滑升级，需重建（`rm` 后重跑）。对单人单库、且已迁移到最新
//! 的项目，这比无限累积迁移代码更划算；早期图谱时代残留的 `entities` 等表本就不在本
//! 文件的建表范围内，亦不处理。

use rusqlite::Connection;

use crate::db::{meta, vec};
use crate::error::Result;

/// 当前 schema 版本号。
pub(crate) const SCHEMA_VERSION: i64 = 19;
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
            role       TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'tool')),
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

        CREATE TABLE IF NOT EXISTS pending_retry (
            id              TEXT PRIMARY KEY,
            session_id      TEXT NOT NULL,
            chat_id         TEXT NOT NULL,
            user_message_id TEXT NOT NULL,
            content         TEXT NOT NULL,
            user_time       TEXT,
            attempts        INTEGER NOT NULL DEFAULT 0,
            next_attempt_at TEXT NOT NULL,
            created_at      TEXT NOT NULL
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
/// 保留的例外只有两处：v17 删掉三张死表 `relations` / `world_state` / `time_profiles`
/// （代码从不读写，纯 schema 残留，删掉不丢信息）；v18 重建 `messages` 以放开
/// `role='tool'`（CHECK 无法 ALTER，只能重建表，见下方）。
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

    // v18：`messages` 放开 `role='tool'`。工具结果此前借用 `role='assistant'` + 非空
    // `tool_call_id` 标记（只为绕开 role CHECK），加载历史时再还原成 `tool`。这里改为
    // 直接落 `role='tool'`，并把历史库里这类行一次性改写过来。
    //
    // SQLite 无法 ALTER 掉 CHECK，只能按官方"重建表"流程走：建新表 → 拷数据（顺带按
    // `tool_call_id` 改写 role）→ 删旧表 → 改名 → 重建索引。**期间关掉外键**，否则
    // `DROP TABLE messages` 会触发 `memories.message_id ON DELETE SET NULL`，把所有
    // 记忆的来源消息关联抹成 NULL。只处理 v17 这一跳：全新库没有 messages 表，由下面的
    // `CREATE TABLE IF NOT EXISTS` 直接建最新结构；更低版本的旧库按约定需重建，不再支持。
    if version == 17 && table_exists(conn, "messages")? {
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             BEGIN;
             CREATE TABLE messages_new (
                 id         TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 role       TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'tool')),
                 content    TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 tool_calls TEXT,
                 tool_call_id TEXT,
                 tool_name TEXT
             );
             INSERT INTO messages_new (id, session_id, role, content, created_at, tool_calls, tool_call_id, tool_name)
                 SELECT id, session_id,
                        CASE WHEN tool_call_id IS NOT NULL THEN 'tool' ELSE role END,
                        content, created_at, tool_calls, tool_call_id, tool_name
                 FROM messages;
             DROP TABLE messages;
             ALTER TABLE messages_new RENAME TO messages;
             CREATE INDEX IF NOT EXISTS idx_messages_session_time ON messages(session_id, created_at);
             COMMIT;
             PRAGMA foreign_keys=ON;",
        )?;
        tracing::info!(
            "迁移 v18：messages 支持 role='tool'，历史工具结果行由 assistant 改写为 tool"
        );
    }

    // v19：新增 `pending_retry` 表，承接"对话轮次因内部错误失败但用户消息已落库"的
    // 跨进程持久重试队列。幂等加表、不碰任何既有业务表，业务数据零丢失。新库由上面的
    // `CREATE TABLE IF NOT EXISTS` 直接建出，这里只为旧库增量补表。
    if version < 19 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_retry (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL,
                chat_id         TEXT NOT NULL,
                user_message_id TEXT NOT NULL,
                content         TEXT NOT NULL,
                user_time       TEXT,
                attempts        INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT NOT NULL,
                created_at      TEXT NOT NULL
            );",
        )?;
        tracing::info!("迁移 v19：新增 pending_retry 表（失败轮次持久重试队列）");
    }

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    tracing::info!("数据库 schema 版本 {version} -> {SCHEMA_VERSION}（增量迁移，业务数据保留）");
    meta::set(conn, KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    Ok(())
}

/// 表是否已存在。
///
/// 迁移里用来区分"全新库"与"旧库"：全新库的 `messages` 尚未建出，重建表分支直接跳过，
/// 由 [`init_schema`] 的 `CREATE TABLE IF NOT EXISTS` 建最新结构。
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(n > 0)
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

    /// v18：messages 放开 `role='tool'`，历史工具结果行由 assistant 改写过来；
    /// 重建表不能破坏 `memories.message_id` 的外键关联，业务数据一条不丢。
    #[test]
    fn v18_migrates_tool_role_and_keeps_memory_links() {
        let conn = open_conn();
        // 复刻生产连接：外键开着，验证 DROP 不会触发 ON DELETE SET NULL。
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 role TEXT NOT NULL CHECK (role IN ('user','assistant')),
                 content TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 tool_calls TEXT,
                 tool_call_id TEXT,
                 tool_name TEXT
             );
             CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 content TEXT NOT NULL,
                 memory_type TEXT NOT NULL CHECK (memory_type IN ('trait','preference','routine','plan')),
                 expires_at TEXT,
                 message_id TEXT REFERENCES messages(id) ON DELETE SET NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             INSERT INTO sessions VALUES ('s1','','t','t');
             INSERT INTO messages (id, session_id, role, content, created_at, tool_call_id, tool_name)
                 VALUES ('m1','s1','user','在吗','t',NULL,NULL);
             INSERT INTO messages (id, session_id, role, content, created_at, tool_call_id, tool_name)
                 VALUES ('m2','s1','assistant','已发送','t','c1','send_message');
             INSERT INTO memories (id, content, memory_type, message_id, created_at, updated_at)
                 VALUES ('mem1','记一件事','trait','m1','t','t');
             INSERT INTO meta (key, value) VALUES ('schema_version','17');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        // 工具结果行改写成 tool，普通行保持原 role。
        let role_of = |id: &str| -> String {
            conn.query_row("SELECT role FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(role_of("m1"), "user");
        assert_eq!(role_of("m2"), "tool", "工具结果行应改为 role='tool'");
        // 新角色可写入（CHECK 已放开）。
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, created_at) VALUES ('m3','s1','tool','ok','t')",
            [],
        )
        .unwrap();
        // 记忆的来源消息关联没被重建表抹掉。
        let linked: Option<String> = conn
            .query_row("SELECT message_id FROM memories WHERE id='mem1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(linked.as_deref(), Some("m1"), "memories.message_id 不能丢");
        assert_eq!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );
    }

    /// v19：增量新增 `pending_retry` 表，不破坏既有业务表。
    #[test]
    fn v19_creates_pending_retry_table() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             INSERT INTO messages VALUES ('m1','s1','user','在吗','t');
             INSERT INTO meta (key, value) VALUES ('schema_version','18');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            table_exists(&conn, "pending_retry").unwrap(),
            "pending_retry 应被 v19 建出"
        );
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
