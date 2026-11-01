//! 业务表 schema 定义与迁移。
//!
//! 当前 schema 完全由 [`init_schema`] 里的 `CREATE TABLE IF NOT EXISTS` 定义（单一真源），
//! 新库打开时直接建最新结构；历史逐版本迁移（v6~v16）已压平，不再保留。
//! 仅 [`migrate`] 里保留 `v17`（删掉三张从未被读写的死表）、`v18`（`messages` 放开
//! `role='tool'` 并改写历史工具结果行）、`v19`（失败轮次重试队列）、`v20`
//! （身份提案 / 版本审计）、`v21`（未版本化身份的提案基线）、`v22`（审批操作者审计）
//! 与 `v23`（运行时配置及审计）
//! 几段，与末尾的版本戳。
//!
//! 取舍：旧版本库不再支持平滑升级，需重建（`rm` 后重跑）。对单人单库、且已迁移到最新
//! 的项目，这比无限累积迁移代码更划算；早期图谱时代残留的 `entities` 等表本就不在本
//! 文件的建表范围内，亦不处理。

use rusqlite::Connection;

use crate::db::{meta, vec};
use crate::error::Result;

/// 当前 schema 版本号。
pub(crate) const SCHEMA_VERSION: i64 = 23;
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

        CREATE TABLE IF NOT EXISTS identity_change_proposals (
            id                   TEXT PRIMARY KEY,
            field_key            TEXT NOT NULL,
            before_value         TEXT,
            after_value          TEXT NOT NULL,
            reason               TEXT NOT NULL,
            evidence             TEXT NOT NULL,
            confidence           TEXT NOT NULL,
            risk_level           TEXT NOT NULL CHECK (risk_level IN ('low', 'medium', 'high')),
            status               TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'applied', 'rolled_back')),
            proposer             TEXT NOT NULL,
            base_version         INTEGER NOT NULL CHECK (base_version >= 0),
            created_at           TEXT NOT NULL,
            decided_at           TEXT,
            decided_by           TEXT,
            applied_at           TEXT,
            rolled_back_at       TEXT
        );

        CREATE TABLE IF NOT EXISTS self_profile_versions (
            version              INTEGER PRIMARY KEY CHECK (version >= 1),
            snapshot             TEXT NOT NULL,
            proposal_id          TEXT,
            source               TEXT NOT NULL CHECK (source IN ('manual', 'evolution_agent', 'rollback')),
            reason               TEXT NOT NULL,
            created_at           TEXT NOT NULL,
            operator             TEXT,
            rolled_back_from     INTEGER
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

        CREATE TABLE IF NOT EXISTS app_settings (
            key        TEXT PRIMARY KEY,
            value_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            updated_by TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_settings_revisions (
            revision    INTEGER PRIMARY KEY AUTOINCREMENT,
            operator    TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            changed_keys TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_settings_audit (
            revision    INTEGER NOT NULL,
            key         TEXT NOT NULL,
            before_json TEXT,
            after_json  TEXT NOT NULL,
            operator    TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            PRIMARY KEY (revision, key)
        );

        CREATE INDEX IF NOT EXISTS idx_messages_session_time ON messages(session_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_memories_expires ON memories(expires_at);
        CREATE INDEX IF NOT EXISTS idx_identity_proposals_status_created
            ON identity_change_proposals(status, created_at);
        CREATE INDEX IF NOT EXISTS idx_identity_proposals_field_status
            ON identity_change_proposals(field_key, status);
        CREATE INDEX IF NOT EXISTS idx_identity_proposals_base_version
            ON identity_change_proposals(base_version);
        CREATE INDEX IF NOT EXISTS idx_self_profile_versions_created
            ON self_profile_versions(created_at);
        CREATE INDEX IF NOT EXISTS idx_app_settings_audit_key_created
            ON app_settings_audit(key, created_at);
        "#,
    )?;

    let cfg = crate::config::Config::get();
    let status = vec::init_vector_table(conn, cfg.embedding_dim, cfg.embedding_model.clone())?;

    vec::ensure_vec_extension(conn)?;

    if let Some(diff) = status.diff_summary() {
        tracing::warn!(
            "记忆向量签名变更：{diff}。程序不会自动重建（memories 的内容/created_at/updated_at 均保留），\
             记忆检索已暂停。确认重建请运行：cargo run --bin zhiwei-cli -- db rebuild"
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

    // v20：身份变更提案与版本审计。只加表与索引，不碰 self_profile 当前快照；
    // 提案的 before/after 与版本快照都由后续身份服务在同一事务内写入。
    if version < 20 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS identity_change_proposals (
                id                   TEXT PRIMARY KEY,
                field_key            TEXT NOT NULL,
                before_value         TEXT,
                after_value          TEXT NOT NULL,
                reason               TEXT NOT NULL,
                evidence             TEXT NOT NULL,
                confidence           TEXT NOT NULL,
                risk_level           TEXT NOT NULL CHECK (risk_level IN ('low', 'medium', 'high')),
                status               TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'applied', 'rolled_back')),
                proposer             TEXT NOT NULL,
                base_version         INTEGER NOT NULL CHECK (base_version >= 0),
                created_at           TEXT NOT NULL,
                decided_at           TEXT,
                applied_at           TEXT,
                rolled_back_at       TEXT
            );

            CREATE TABLE IF NOT EXISTS self_profile_versions (
                version              INTEGER PRIMARY KEY CHECK (version >= 1),
                snapshot             TEXT NOT NULL,
                proposal_id          TEXT,
                source               TEXT NOT NULL CHECK (source IN ('manual', 'evolution_agent', 'rollback')),
                reason               TEXT NOT NULL,
                created_at           TEXT NOT NULL,
                rolled_back_from     INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_identity_proposals_status_created
                ON identity_change_proposals(status, created_at);
            CREATE INDEX IF NOT EXISTS idx_identity_proposals_field_status
                ON identity_change_proposals(field_key, status);
            CREATE INDEX IF NOT EXISTS idx_identity_proposals_base_version
                ON identity_change_proposals(base_version);
            CREATE INDEX IF NOT EXISTS idx_self_profile_versions_created
                ON self_profile_versions(created_at);",
        )?;
        tracing::info!("迁移 v20：新增身份变更提案与版本审计表");
    }

    // v21：提案的 `base_version` 允许 0，表示当前身份仍是未写入版本审计表的旧 KV。
    // 真实库的提案表为空，因此直接重建表最干净；若已有数据，原样迁移，不丢审计。
    if version < 21 && table_exists(conn, "identity_change_proposals")? {
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             BEGIN;
             CREATE TABLE identity_change_proposals_new (
                 id                   TEXT PRIMARY KEY,
                 field_key            TEXT NOT NULL,
                 before_value         TEXT,
                 after_value          TEXT NOT NULL,
                 reason               TEXT NOT NULL,
                 evidence             TEXT NOT NULL,
                 confidence           TEXT NOT NULL,
                 risk_level           TEXT NOT NULL CHECK (risk_level IN ('low', 'medium', 'high')),
                 status               TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'applied', 'rolled_back')),
                 proposer             TEXT NOT NULL,
                 base_version         INTEGER NOT NULL CHECK (base_version >= 0),
                 created_at           TEXT NOT NULL,
                 decided_at           TEXT,
                 applied_at           TEXT,
                 rolled_back_at       TEXT
             );
             INSERT INTO identity_change_proposals_new
                 SELECT id, field_key, before_value, after_value, reason, evidence,
                        confidence, risk_level, status, proposer, base_version,
                        created_at, decided_at, applied_at, rolled_back_at
                 FROM identity_change_proposals;
             DROP TABLE identity_change_proposals;
             ALTER TABLE identity_change_proposals_new RENAME TO identity_change_proposals;
             CREATE INDEX IF NOT EXISTS idx_identity_proposals_status_created
                 ON identity_change_proposals(status, created_at);
             CREATE INDEX IF NOT EXISTS idx_identity_proposals_field_status
                 ON identity_change_proposals(field_key, status);
             CREATE INDEX IF NOT EXISTS idx_identity_proposals_base_version
                 ON identity_change_proposals(base_version);
             COMMIT;
             PRAGMA foreign_keys=ON;",
        )?;
        tracing::info!("迁移 v21：身份提案允许 base_version=0（未版本化身份）");
    }

    // v22：把审批人 / 版本操作者写入审计。提案保留 `decided_by`，版本保留 `operator`；
    // 旧行允许为空，因为历史上没有记录操作者。新库也走这里补列，保持建表与迁移一致。
    if version < 22 {
        if table_exists(conn, "identity_change_proposals")?
            && !column_exists(conn, "identity_change_proposals", "decided_by")?
        {
            conn.execute_batch(
                "ALTER TABLE identity_change_proposals ADD COLUMN decided_by TEXT;",
            )?;
        }
        if table_exists(conn, "self_profile_versions")?
            && !column_exists(conn, "self_profile_versions", "operator")?
        {
            conn.execute_batch("ALTER TABLE self_profile_versions ADD COLUMN operator TEXT;")?;
        }
        tracing::info!("迁移 v22：身份提案与版本审计新增操作者字段");
    }

    // v23：运行时配置落库。配置值、revision 与逐字段审计都放在独立表；
    // 新库由 init_schema 直接建出，旧库在这里幂等补表，不碰任何业务数据。
    if version < 23 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_settings (
                key        TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                updated_by TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS app_settings_revisions (
                revision    INTEGER PRIMARY KEY AUTOINCREMENT,
                operator    TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                changed_keys TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS app_settings_audit (
                revision    INTEGER NOT NULL,
                key         TEXT NOT NULL,
                before_json TEXT,
                after_json  TEXT NOT NULL,
                operator    TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                PRIMARY KEY (revision, key)
            );
            CREATE INDEX IF NOT EXISTS idx_app_settings_audit_key_created
                ON app_settings_audit(key, created_at);",
        )?;
        tracing::info!("迁移 v23：新增运行时配置与审计表");
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

/// 列是否已存在；用于让增量迁移可重试，避免半完成迁移再次执行时 ALTER 失败。
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
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

    #[test]
    fn v23_adds_runtime_settings_without_touching_business_rows() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             INSERT INTO sessions VALUES ('keep', '保留', 't', 't');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '22')",
            [],
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(stored_version(&conn).unwrap().as_deref(), Some("23"));
        assert!(table_exists(&conn, "app_settings").unwrap());
        assert!(table_exists(&conn, "app_settings_revisions").unwrap());
        assert!(table_exists(&conn, "app_settings_audit").unwrap());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions WHERE id = 'keep'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "业务数据必须保留");
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

    /// v20：增量新增身份提案与版本审计表，既有 self_profile / messages 不动。
    #[test]
    fn v20_creates_identity_tables_and_preserves_profile() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE self_profile (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             INSERT INTO self_profile VALUES ('性格','慢热','t');
             INSERT INTO messages VALUES ('m1','s1','user','在吗','t');
             INSERT INTO meta (key, value) VALUES ('schema_version','19');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            table_exists(&conn, "identity_change_proposals").unwrap(),
            "identity_change_proposals 应被 v20 建出"
        );
        assert!(
            table_exists(&conn, "self_profile_versions").unwrap(),
            "self_profile_versions 应被 v20 建出"
        );
        let kept: String = conn
            .query_row(
                "SELECT value FROM self_profile WHERE key='性格'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "慢热", "self_profile 当前快照不能丢");
        let messages: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 1, "messages 一条不能少");
        assert_eq!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );
    }

    /// v20 的 CHECK 约束必须挡住非法风险等级 / 状态 / 来源；后续 DAO 的错误不能是
    /// 唯一防线，直接改库也要被 schema 拒绝。
    #[test]
    fn v20_constraints_reject_invalid_enums() {
        let conn = open_conn();
        migrate(&conn).unwrap();

        let proposal = conn.execute(
            "INSERT INTO identity_change_proposals
             (id, field_key, before_value, after_value, reason, evidence, confidence,
              risk_level, status, proposer, base_version, created_at, decided_by)
             VALUES ('p1','voice_style',NULL,'更短','测试','[]','medium',
                     'critical','pending','test',1,'t',NULL)",
            [],
        );
        assert!(proposal.is_err(), "非法 risk_level 应被 CHECK 拒绝");

        let proposal = conn.execute(
            "INSERT INTO identity_change_proposals
             (id, field_key, before_value, after_value, reason, evidence, confidence,
              risk_level, status, proposer, base_version, created_at, decided_by)
             VALUES ('p2','voice_style',NULL,'更短','测试','[]','medium',
                     'low','done','test',1,'t',NULL)",
            [],
        );
        assert!(proposal.is_err(), "非法 status 应被 CHECK 拒绝");

        let version = conn.execute(
            "INSERT INTO self_profile_versions
             (version, snapshot, proposal_id, source, reason, created_at, operator,
              rolled_back_from)
             VALUES (1,'{}',NULL,'model','测试','t',NULL,NULL)",
            [],
        );
        assert!(version.is_err(), "非法 source 应被 CHECK 拒绝");
    }

    /// v21：旧 KV 身份没有版本记录，提案必须能用 `base_version=0` 表达这一事实，
    /// 且迁移不能丢掉已有提案。
    #[test]
    fn v21_allows_unversioned_base_and_preserves_proposals() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE identity_change_proposals (
                id TEXT PRIMARY KEY,
                field_key TEXT NOT NULL,
                before_value TEXT,
                after_value TEXT NOT NULL,
                reason TEXT NOT NULL,
                evidence TEXT NOT NULL,
                confidence TEXT NOT NULL,
                risk_level TEXT NOT NULL CHECK (risk_level IN ('low', 'medium', 'high')),
                status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'applied', 'rolled_back')),
                proposer TEXT NOT NULL,
                base_version INTEGER NOT NULL CHECK (base_version >= 1),
                created_at TEXT NOT NULL,
                decided_at TEXT,
                applied_at TEXT,
                rolled_back_at TEXT
            );
            INSERT INTO identity_change_proposals
                (id, field_key, after_value, reason, evidence, confidence, risk_level,
                 status, proposer, base_version, created_at)
                VALUES ('p1','voice_style','更短','测试','[\"m1\"]','medium',
                        'low','pending','test',1,'t');
            INSERT INTO meta (key, value) VALUES ('schema_version','20');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM identity_change_proposals WHERE id='p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "迁移不能丢已有提案");

        conn.execute(
            "INSERT INTO identity_change_proposals
             (id, field_key, after_value, reason, evidence, confidence, risk_level,
              status, proposer, base_version, created_at, decided_by)
             VALUES ('p2','voice_style','更短','测试','[\"m1\"]','medium',
                     'low','pending','test',0,'t',NULL)",
            [],
        )
        .unwrap();
        assert_eq!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );
    }

    /// v22：操作者字段可重复补列，旧行留空，且不丢既有提案 / 版本。
    #[test]
    fn v22_adds_operator_columns_without_touching_existing_audit_rows() {
        let conn = open_conn();
        // 先模拟 v21 旧表，避免新库建表时已经带上了操作者列。
        conn.execute_batch(
            "CREATE TABLE identity_change_proposals (
                id TEXT PRIMARY KEY,
                field_key TEXT NOT NULL,
                before_value TEXT,
                after_value TEXT NOT NULL,
                reason TEXT NOT NULL,
                evidence TEXT NOT NULL,
                confidence TEXT NOT NULL,
                risk_level TEXT NOT NULL CHECK (risk_level IN ('low', 'medium', 'high')),
                status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'applied', 'rolled_back')),
                proposer TEXT NOT NULL,
                base_version INTEGER NOT NULL CHECK (base_version >= 0),
                created_at TEXT NOT NULL,
                decided_at TEXT,
                applied_at TEXT,
                rolled_back_at TEXT
            );
            CREATE TABLE self_profile_versions (
                version INTEGER PRIMARY KEY CHECK (version >= 1),
                snapshot TEXT NOT NULL,
                proposal_id TEXT,
                source TEXT NOT NULL CHECK (source IN ('manual', 'evolution_agent', 'rollback')),
                reason TEXT NOT NULL,
                created_at TEXT NOT NULL,
                rolled_back_from INTEGER
            );
            INSERT INTO meta (key, value) VALUES ('schema_version','21');",
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert!(
            column_exists(&conn, "identity_change_proposals", "decided_by").unwrap(),
            "提案表应有审批人字段"
        );
        assert!(
            column_exists(&conn, "self_profile_versions", "operator").unwrap(),
            "版本表应有操作者字段"
        );

        conn.execute(
            "INSERT INTO identity_change_proposals
             (id, field_key, after_value, reason, evidence, confidence, risk_level,
              status, proposer, base_version, created_at, decided_by)
             VALUES ('p1','voice_style','更短','测试','[\"m1\"]','medium',
                     'low','pending','test',0,'t',NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO self_profile_versions
             (version, snapshot, proposal_id, source, reason, created_at, operator)
             VALUES (1,'{}',NULL,'manual','测试','t',NULL)",
            [],
        )
        .unwrap();
        let proposal_operator: Option<String> = conn
            .query_row(
                "SELECT decided_by FROM identity_change_proposals WHERE id='p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let version_operator: Option<String> = conn
            .query_row(
                "SELECT operator FROM self_profile_versions WHERE version=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(proposal_operator, None, "历史提案没有审批人，应允许为空");
        assert_eq!(version_operator, None, "历史版本没有操作者，应允许为空");

        // 半完成迁移 / 手工建表重复执行也必须幂等。
        migrate(&conn).unwrap();
    }
}
