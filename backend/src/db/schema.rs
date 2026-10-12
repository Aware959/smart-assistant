//! 业务表 schema 定义与增量迁移。
//!
//! 迁移默认**只做增量**（新增表/列）：memories / messages / sessions 一旦落库即永久
//! 保留；旧版残留的表（如图谱时代的 entities/relations）不删除，只是不再读写。
//!
//! 例外：
//! - v12：为了给 `memories.memory_type` 加 CHECK 约束，必须整表重建（SQLite 不支持
//!   ALTER TABLE ADD CONSTRAINT）。它是**保数据的重建**——单事务、搬迁前后校验行数、
//!   中途失败自动回滚，且逐行保留 content / created_at / updated_at。
//! - v13：DROP `intent_state`。这张表存的每个值都能从 `messages` 推出来，删掉不丢信息。
//!   任何新增的重建都要照 v12 的标准写。
//! - v15：DROP COLUMN `messages.intent`。该列是"每日主动配额"方案（v14）加进来的，方案
//!   回退后没有任何代码读写它（值全 0），用删列迁移清掉残留。
//! - v16：messages 增量补三列 `tool_calls` / `tool_call_id` / `tool_name`，持久化工具调用
//!   上下文。工具结果沿用 `role='assistant'` + `tool_call_id` 标记（不新增 `'tool'` 角色，
//!   避免改 CHECK 约束触发整表重建）。

use rusqlite::Connection;

use crate::db::{meta, vec};
use crate::error::{Result, SqlError};

/// 当前 schema 版本号。
pub(crate) const SCHEMA_VERSION: i64 = 16;
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
/// 旧版残留的表（如图谱时代的 entities/relations）不删除，只是不再读写。
///
/// 三处例外：[`rebuild_memories_with_kind`]（v12）为 `memory_type` 加 CHECK 约束而整表
/// 重建（保数据重建：单事务 + 行数校验 + 失败回滚）；v13 删掉 `intent_state`——它的每个
/// 值都能从 `messages` 推出来，删掉不丢信息；v15 删掉 `messages.intent` 残留列。
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
            conn.execute_batch("ALTER TABLE proactive_state RENAME TO intent_state;")?;
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

    if version < 11 {
        // v11：messages 改用 (session_id, created_at) 复合索引。
        // 历史单列索引 (session_id) 已被它完全覆盖（最左前缀即可服务
        // "按会话过滤"），留着只是每条消息多维护一棵 B 树。
        conn.execute_batch("DROP INDEX IF EXISTS idx_messages_session;")?;
    }

    if version < 12 {
        // v12：记忆种类枚举化（memory_type 收窄为 trait/preference/routine/plan
        // 并加 CHECK 约束），同时删掉 tier 列——它和 expires_at 是同一个事实的两种
        // 表示，且从 v6 起就没有任何一处读它；留着只会出现"tier 说 core 但
        // expires_at 是明天"这种自相矛盾。
        //
        // CHECK 约束只能建在 CREATE TABLE 上，SQLite 又不支持 ALTER TABLE ADD
        // CONSTRAINT，所以这是本文件唯一一次重建业务表：新建 memories_v12 →
        // 逐行搬 → 换名，全程一个事务，中途失败原表原样不动。
        rebuild_memories_with_kind(conn)?;
    }

    if version < 13 {
        // v13：删掉 intent_state 表。主动开口的时间账本（最近一次入站 / 最近一次
        // assistant 消息）直接由 messages 表派生，不再需要一张靠写穿维护的副本：
        // 抄写路径一旦漏掉就会和真实时间线漂移，而它存的每一个值 messages 里都有。
        // 这是本文件第二次 DROP 表——与 entities / relations 不同，intent_state 从未被
        // 任何业务逻辑读作真值来源，删掉不丢信息。
        conn.execute_batch("DROP TABLE IF EXISTS intent_state;")?;
    }

    if version < 15 {
        // v15：删掉 `messages.intent` 残留列。
        // 该列是"每日主动配额"方案在 v14 加进来的，方案整体回退后没有任何代码读写
        // 它（值全为 0），从真实库清干净。新版库的 CREATE TABLE 本来就不带这一列，
        // 这里只服务带残留列的老库——表/列不存在时跳过（SQLite DROP COLUMN 需
        // 3.35+，随附版本满足）。
        if column_exists(conn, "messages", "intent")? {
            conn.execute_batch("ALTER TABLE messages DROP COLUMN intent;")?;
            tracing::info!("迁移 v15：删除 messages.intent 残留列");
        }
    }

    if version < 16 {
        // v16：messages 增三列以持久化工具调用上下文（assistant 轮的工具调用
        // 列表、tool 结果的调用 id 与工具名），让跨轮 / 重启后的历史能完整拼回
        // 工具往返。沿用 `role='assistant'` + `tool_call_id` 标记工具结果，而非
        // 新增 `'tool'` 角色——后者要改动 role 的 CHECK 约束，而 SQLite 不支持
        // ALTER TABLE ADD CONSTRAINT，只能整表重建（v12 那套保数据重建有风险且
        // 收益仅在于 DB 里角色名更直白）。工具结果靠 `tool_call_id` 非空来识别，
        // 加载历史时再还原成 genai 需要的 role='tool'。三列可空、纯增量补列。
        add_column_if_missing(conn, "messages", "tool_calls", "TEXT")?;
        add_column_if_missing(conn, "messages", "tool_call_id", "TEXT")?;
        add_column_if_missing(conn, "messages", "tool_name", "TEXT")?;
    }

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    tracing::info!("数据库 schema 版本 {version} -> {SCHEMA_VERSION}（增量迁移，业务数据保留）");
    meta::set(conn, KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    Ok(())
}

/// v12：重建 `memories`，把 `memory_type` 收窄成 [`MemoryKind`](crate::db::memory::MemoryKind)
/// 并加上 CHECK 约束，丢弃 `tier` 列。
///
/// 旧库的 `memory_type` 是历史分类器留下的自由文本（personal / preference / fact /
/// intent / todo），只能**近似**映射到新种类：语义最接近的映射是有损的，所以逐行
/// 打印映射结果，留给使用者核对。真正的精确归类只能由内容判断决定，那是数据层的
/// 一次性工作，不该塞进通用迁移里。
///
/// 到期时间同时按种类补齐：有锚点的旧行保留原值；`routine` / `plan` 且原本永不过期
/// 的行补上默认窗口，否则"没落实的意向"会在迁移后变成永久事实。
fn rebuild_memories_with_kind(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "memories")? {
        // 全新库：init_schema 的 CREATE TABLE 稍后会直接建出带 CHECK 的新表。
        return Ok(());
    }
    let legacy = LegacyKindMap::collect(conn)?;
    if legacy.0.is_empty() {
        tracing::info!("迁移 v12：memories 无数据，跳过重建");
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(
        r#"
        CREATE TABLE memories_v12 (
            id          TEXT PRIMARY KEY,
            content     TEXT NOT NULL,
            memory_type TEXT NOT NULL CHECK (memory_type IN ('trait', 'preference', 'routine', 'plan')),
            expires_at  TEXT,
            message_id  TEXT REFERENCES messages(id) ON DELETE SET NULL,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        "#,
    )?;

    // routine=90 天、plan=30 天：与 MemoryKind::default_lifetime_days 保持一致。
    // 这里写死天数而不是调枚举方法——迁移要在新列上算出 expires_at，
    // 而 SQL 表达式里调不了 Rust。
    tx.execute_batch(
        r#"
        INSERT INTO memories_v12 (id, content, memory_type, expires_at, message_id, created_at, updated_at)
        SELECT id,
               content,
               CASE memory_type
                   WHEN 'preference' THEN 'preference'
                   WHEN 'routine'    THEN 'routine'
                   WHEN 'plan'       THEN 'plan'
                   WHEN 'personal'   THEN 'trait'
                   WHEN 'fact'       THEN 'trait'
                   WHEN 'todo'       THEN 'plan'
                   WHEN 'intent'     THEN 'plan'
                   ELSE 'trait'
               END,
               CASE
                   -- 已有到期日的一律原样保留：那是当初按内容里的时间锚算出来的，
                   -- 比按种类重算更贴近那句话本身（过期的那条保留过去时间即可，
                   -- 它本来就不再被召回，重算反而会把过期的意向复活）。
                   WHEN expires_at IS NOT NULL THEN expires_at
                   -- 没到期日的按种类补默认窗口（与 MemoryKind::default_lifetime_days 一致：
                   -- routine 90 天、plan 30 天，trait / preference 到这里就是 NULL）。
                   WHEN memory_type IN ('routine', 'plan', 'todo', 'intent')
                       THEN strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now',
                                     CASE WHEN memory_type = 'routine' THEN '+90 days' ELSE '+30 days' END)
                   ELSE NULL
               END,
               message_id,
               created_at,
               updated_at
          FROM memories;
        "#,
    )?;

    let moved: i64 = tx.query_row("SELECT COUNT(*) FROM memories_v12", [], |r| r.get(0))?;
    let before: i64 = tx.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
    if moved != before {
        return Err(SqlError::Config(format!(
            "v12 迁移行数不符：源 {before} 行，目标 {moved} 行，已回滚"
        )));
    }

    tx.execute_batch(
        r#"
        DROP TABLE memories;
        ALTER TABLE memories_v12 RENAME TO memories;
        CREATE INDEX IF NOT EXISTS idx_memories_expires ON memories(expires_at);
        "#,
    )?;

    tx.commit()?;

    tracing::info!("迁移 v12：memories 重建完成，{moved} 行；旧 memory_type 映射 {legacy}");
    Ok(())
}

/// 迁移前的旧 `memory_type` 取值分布，用于事后核对映射结果。
struct LegacyKindMap(Vec<(String, i64)>);

impl LegacyKindMap {
    fn collect(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare(
            "SELECT memory_type, COUNT(*) FROM memories GROUP BY memory_type ORDER BY memory_type",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self(rows))
    }
}

impl std::fmt::Display for LegacyKindMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|(t, n)| format!("{t}×{n}")).collect();
        write!(f, "{}", parts.join(", "))
    }
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

    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def};"))?;
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

    use chrono::Utc;

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
        // messages 要在：v12 重建 memories 时会带上 message_id 外键。
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

    /// v12：旧 `memory_type` 映射到四种类、`tier` 列消失、CHECK 真的挡住表外值。
    #[test]
    fn v12_narrows_memory_type_and_drops_tier() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             CREATE TABLE memories (
                 id          TEXT PRIMARY KEY,
                 content     TEXT NOT NULL,
                 memory_type TEXT NOT NULL DEFAULT 'fact',
                 tier        TEXT NOT NULL DEFAULT 'core',
                 expires_at  TEXT,
                 message_id  TEXT,
                 created_at  TEXT NOT NULL,
                 updated_at  TEXT NOT NULL
             );
             INSERT INTO memories (id, content, memory_type, tier, expires_at, message_id, created_at, updated_at)
             VALUES
               ('a', '会一点厨艺',       'personal',   'core',  NULL,                             NULL, 'c', 'u'),
               ('b', '希望被叫阿离',     'preference', 'core',  NULL,                             NULL, 'c', 'u'),
               ('c', '午休 12 点到 2 点', 'routine',    'short', NULL,                             NULL, 'c', 'u'),
               ('d', '下周要体检',       'intent',     'short', '2000-01-01T00:00:00+00:00',      NULL, 'c', 'u'),
               ('e', '打算看电影',       'todo',       'short', NULL,                             NULL, 'c', 'u'),
               ('f', '住在上海',         'mystery',    'core',  NULL,                             NULL, 'c', 'u');
             INSERT INTO meta (key, value) VALUES ('schema_version', '11');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            !column_exists(&conn, "memories", "tier").unwrap(),
            "tier 列应被 v12 删掉"
        );

        let rows: Vec<(String, String, Option<String>)> = {
            let mut stmt = conn
                .prepare("SELECT id, memory_type, expires_at FROM memories ORDER BY id")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(rows.len(), 6, "重建不能丢行");

        let kind = |id: &str| {
            rows.iter()
                .find(|(i, _, _)| i == id)
                .map(|(_, k, _)| k.as_str())
                .unwrap_or_else(|| panic!("{id} 应还在"))
        };
        assert_eq!(kind("a"), "trait", "personal → trait");
        assert_eq!(kind("b"), "preference");
        assert_eq!(kind("c"), "routine");
        assert_eq!(kind("d"), "plan", "intent → plan");
        assert_eq!(kind("e"), "plan", "todo → plan");
        assert_eq!(kind("f"), "trait", "表外值也不能挡住整次迁移");

        // 有锚点/原到期日的行保留原值；routine/plan 缺到期日才补默认窗口。
        let expires = |id: &str| {
            rows.iter()
                .find(|(i, _, _)| i == id)
                .and_then(|(_, _, e)| e.clone())
        };
        assert_eq!(expires("a"), None, "trait 永不过期");
        assert_eq!(expires("b"), None, "preference 永不过期");
        assert_eq!(
            expires("d").as_deref(),
            Some("2000-01-01T00:00:00+00:00"),
            "已有到期日应原样保留，不按种类重算"
        );
        for id in ["c", "e"] {
            let e = expires(id).unwrap_or_else(|| panic!("{id} 应补默认到期日"));
            assert!(
                e > Utc::now().to_rfc3339(),
                "{id} 的补齐到期日应在将来：{e}"
            );
        }

        // CHECK 约束真的存在：绕过 Rust 直接写表外值必须失败。
        let illegal = conn.execute(
            "INSERT INTO memories (id, content, memory_type, expires_at, message_id, created_at, updated_at)
             VALUES ('x', '瞎写的', 'core', NULL, NULL, 'c', 'u')",
            [],
        );
        assert!(illegal.is_err(), "表外 memory_type 应被 CHECK 挡住");
    }

    /// v9 的改名在完整迁移链里只是中间步骤：v13 会把 `intent_state` 整个删掉
    /// （时间账本改由 `messages` 派生）。所以旧表的最终归宿是"两张都不剩"。
    #[test]
    fn v9_legacy_proactive_table_is_dropped_by_v13() {
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
        assert!(
            !table_exists(&conn, "intent_state").unwrap(),
            "v13 应删掉 intent_state"
        );
    }

    /// v13：删掉 `intent_state`。删表不丢信息——它存的每个值都能从 `messages` 推出
    /// （见 `channel::list_proactive_rows` 的测试），消息本身一条不少。
    #[test]
    fn v13_drops_intent_state_and_keeps_messages() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             CREATE TABLE intent_state (
                 channel TEXT NOT NULL, external_id TEXT NOT NULL, session_id TEXT NOT NULL,
                 last_user_reply_at TEXT, last_intent_at TEXT,
                 today_count INTEGER NOT NULL DEFAULT 0, today_date TEXT,
                 PRIMARY KEY (channel, external_id)
             );
             INSERT INTO intent_state (channel, external_id, session_id, last_user_reply_at)
             VALUES ('telegram', '100', 's1', '2026-01-01T00:00:00Z');
             INSERT INTO messages VALUES
                 ('m1', 's1', 'user',      '在吗',   '2026-01-01T09:00:00+00:00'),
                 ('m2', 's1', 'assistant', '在的',   '2026-01-01T09:00:05+00:00'),
                 ('m3', 's1', 'user',      '吃了吗', '2026-01-01T10:30:00+00:00');
             INSERT INTO meta (key, value) VALUES ('schema_version', '12');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            !table_exists(&conn, "intent_state").unwrap(),
            "intent_state 应被 v13 删掉"
        );
        assert_eq!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 3, "messages 一条不能少");
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
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap() == SCHEMA_VERSION.to_string()
        );
        let content: String = conn
            .query_row(
                "SELECT content FROM messages WHERE id = 'keep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content, "别丢");
    }

    /// v15：删掉 `messages.intent` 残留列（某次方案在 v14 加列、回退后没删）。
    /// 列删干净，消息一条不少；无该列的老库（从未加过）迁移也不报错。
    #[test]
    fn v15_drops_leftover_intent_column() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (
                 id         TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 role       TEXT NOT NULL,
                 content    TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 intent     INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO messages (id, session_id, role, content, created_at, intent) VALUES
                 ('m1', 's1', 'user',      '在吗', '2026-01-01T09:00:00+00:00', 0),
                 ('m2', 's1', 'assistant', '在的', '2026-01-01T09:00:05+00:00', 1);
             INSERT INTO meta (key, value) VALUES ('schema_version', '14');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            !column_exists(&conn, "messages", "intent").unwrap(),
            "intent 列应被 v15 删掉"
        );
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 2, "messages 一条不能少");
    }

    /// v16：messages 增量补 `tool_calls` / `tool_call_id` / `tool_name` 三列，旧消息不丢，
    /// 工具结果行（role='assistant' + tool_call_id 标记）可正常落库。
    #[test]
    fn v16_adds_tool_columns_to_messages() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL,
                 content TEXT NOT NULL, created_at TEXT NOT NULL
             );
             INSERT INTO messages VALUES ('m1','s1','user','在吗','2026-01-01T09:00:00+00:00');
             INSERT INTO messages VALUES ('m2','s1','assistant','在的','2026-01-01T09:00:05+00:00');
             INSERT INTO meta (key, value) VALUES ('schema_version','15');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert_eq!(
            meta::get(&conn, KEY_SCHEMA_VERSION).unwrap().unwrap(),
            SCHEMA_VERSION.to_string()
        );
        // 三列已补，可空。
        assert!(
            column_exists(&conn, "messages", "tool_calls").unwrap(),
            "tool_calls 应已补列"
        );
        assert!(
            column_exists(&conn, "messages", "tool_call_id").unwrap(),
            "tool_call_id 应已补列"
        );
        assert!(
            column_exists(&conn, "messages", "tool_name").unwrap(),
            "tool_name 应已补列"
        );
        // 旧消息一条不少。
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 2, "旧消息不能丢");
        // 工具结果行可落库（role='assistant' + tool_call_id 标记，沿用避免改 role CHECK）。
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, created_at, tool_call_id, tool_name)
             VALUES ('t1','s1','assistant','结果','2026-01-01T09:00:06+00:00','c1','recall_memory')",
            [],
        )
        .unwrap();
        let (cid, name): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT tool_call_id, tool_name FROM messages WHERE id='t1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cid.as_deref(), Some("c1"));
        assert_eq!(name.as_deref(), Some("recall_memory"));
    }

    /// v11：旧库（带历史单列索引）迁移后换成 (session_id, created_at) 复合索引，
    /// 业务数据不动。
    #[test]
    fn v11_replaces_message_index_with_composite() {
        let conn = open_conn();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT);
             CREATE INDEX idx_messages_session ON messages(session_id);
             INSERT INTO messages (id, session_id, role, content, created_at)
             VALUES ('keep', 's1', 'user', '别丢', 't');
             INSERT INTO meta (key, value) VALUES ('schema_version', '10');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(
            !index_exists(&conn, "idx_messages_session").unwrap(),
            "单列索引应被复合索引取代"
        );
        let content: String = conn
            .query_row(
                "SELECT content FROM messages WHERE id = 'keep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content, "别丢");
    }

    /// 索引是否已存在（sqlite_master）。
    fn index_exists(conn: &Connection, name: &str) -> Result<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }
}
