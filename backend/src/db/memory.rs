use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{Result, SqlError};

/// 记忆的**种类**——"这条关于对方的陈述属于哪一类"，也是"什么才允许写进记忆库"
/// 的白名单。枚举而不是自由文本：边界的价值全在"名单短且每项有名字"，散成一段
/// 提示词就既审不了也测不了。
///
/// 寿命不在这四个值里，而在 [`MemoryKind::default_lifetime_days`]：
/// 种类的默认寿命作为**枚举上的数据**存在，而不是散落的 env 配置——否则调整一个
/// 默认期限要同时改配置项、改解析、改默认值三处，而其中一处没人会记得。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// 稳定的性格 / 能力 / 身份事实："会一点厨艺"、"自称只是傲娇"、"名字是阿离"。
    Trait,
    /// 稳定的偏好与对 AI 的指令："希望被称呼为阿离"、"想念时大胆说出来"。
    Preference,
    /// 作息与时段规律："午休时间是中午12点到下午2点"。
    Routine,
    /// 有时效的意向与安排："打算下个月去看电影"。没有落实就该忘掉。
    Plan,
}

impl MemoryKind {
    /// 全部种类，供枚举遍历 / 工具 schema 生成 / 迁移使用。
    pub const ALL: [MemoryKind; 4] = [
        MemoryKind::Trait,
        MemoryKind::Preference,
        MemoryKind::Routine,
        MemoryKind::Plan,
    ];

    /// 落库 / 出参用的字符串（小写）。
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Trait => "trait",
            MemoryKind::Preference => "preference",
            MemoryKind::Routine => "routine",
            MemoryKind::Plan => "plan",
        }
    }

    /// 解析落库字符串。**未知值不静默兜底成某个种类**，而是原样带回
    /// [`SqlError::Config`]——库里出现表外的值意味着迁移漏了或有人手改过表，
    /// 猜一个类别会把错数据洗成"看起来正常"，比报错难查得多。
    pub fn parse(raw: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == raw.trim().to_ascii_lowercase())
            .ok_or_else(|| {
                SqlError::Config(format!(
                    "未知的记忆种类：{raw}（合法值：{}）",
                    Self::ALL.map(|k| k.as_str()).join(" / ")
                ))
            })
    }

    /// 该种类的默认寿命（天）。`None` = 长期有效，变更靠模型 `supersedes` 改写，
    /// 不靠时间冲淡。
    ///
    /// 刻意不给 `trait` / `preference` 设期限：它们定义"对方是谁、该被怎么对待"，
    /// 过期只会让 AI 在对方没变的情况下失忆。给 `routine` / `plan` 设窗口，则是为了
    /// 不让"最近胃不舒服""打算换工作"这类没有明确时间锚的陈述变成永久事实——
    /// 没有锚点就没有精确到期日，只能给一个有界窗口兜底。
    pub fn default_lifetime_days(self) -> Option<i64> {
        match self {
            MemoryKind::Trait | MemoryKind::Preference => None,
            // 作息会变：90 天内没被再次确认，就当它不成立了（重新确认会刷新期限）。
            MemoryKind::Routine => Some(90),
            // 意向不落实就是过期信息：给 30 天；有明确时间锚时由锚点算出精确到期日。
            MemoryKind::Plan => Some(30),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    /// 种类（[`MemoryKind`]）；它同时决定这条记忆的默认寿命。
    pub memory_type: MemoryKind,
    /// 过期时间（RFC3339）；None 表示长期记忆，永不失效。
    pub expires_at: Option<String>,
    /// 来源消息 id（对话中被沉淀的输入），无则说明是手动添加。
    pub message_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Memory {
    pub fn new(
        content: String,
        memory_type: MemoryKind,
        message_id: Option<String>,
        expires_at: Option<String>,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            memory_type,
            expires_at,
            message_id,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

pub fn create_record(db: &Database, memory: &Memory) -> Result<()> {
    db.conn().execute(
        "INSERT INTO memories (id, content, memory_type, expires_at, message_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            memory.id,
            memory.content,
            memory.memory_type.as_str(),
            memory.expires_at,
            memory.message_id,
            memory.created_at,
            memory.updated_at
        ],
    )?;
    Ok(())
}

pub fn create(db: &Database, memory: &Memory, embedding: &[f32]) -> Result<()> {
    create_record(db, memory)?;
    insert_vector(db, &memory.id, embedding)
}

/// 仅写入向量（供重建时按 memories.content 回填）。
pub fn insert_vector(db: &Database, memory_id: &str, embedding: &[f32]) -> Result<()> {
    let bytes = to_byte_array(embedding);
    db.conn().execute(
        "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![memory_id, bytes.as_slice()],
    )?;
    Ok(())
}

pub fn get(db: &Database, id: &str) -> Result<Memory> {
    let row = db.conn().query_row(
        "SELECT id, content, memory_type, expires_at, message_id, created_at, updated_at
         FROM memories WHERE id = ?1",
        [id],
        map_memory_row,
    )?;
    Ok(row)
}

/// 原地改写一条记忆：内容、种类、过期时间一并更新，向量同步重写。
///
/// **保留 `id` 与 `created_at`**——记忆的身份不变，只是内容被修正。"取代一条旧事实"
/// 和"新增一条新事实"的区别正在于此：不能让库里同时躺着两个互相矛盾的说法，
/// 也不能让召回方看到已经作废的旧版本。
///
/// `new_embedding` 为空时跳过向量改写（向量表不可用时的降级路径）。
pub fn revise(
    db: &Database,
    id: &str,
    content: &str,
    memory_type: MemoryKind,
    expires_at: Option<&str>,
    new_embedding: &[f32],
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    db.conn().execute(
        "UPDATE memories SET content = ?1, memory_type = ?2, expires_at = ?3, updated_at = ?4 WHERE id = ?5",
        params![content, memory_type.as_str(), expires_at, now, id],
    )?;

    if !new_embedding.is_empty() {
        let bytes = to_byte_array(new_embedding);
        db.conn().execute(
            "UPDATE memory_vectors SET embedding = ?2 WHERE memory_id = ?1",
            rusqlite::params![id, bytes.as_slice()],
        )?;
    }
    Ok(())
}

pub fn delete(db: &Database, id: &str) -> Result<()> {
    let tx = db.conn().unchecked_transaction()?;
    tx.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [id])?;
    tx.execute("DELETE FROM memories WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn list(db: &Database, limit: usize) -> Result<Vec<Memory>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, content, memory_type, expires_at, message_id, created_at, updated_at
         FROM memories ORDER BY created_at DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit as i64], map_memory_row)?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// 行映射里用：把 [`MemoryKind::parse`] 的错误转成 rusqlite 错误，让闭包签名保持
/// `rusqlite::Result<…>`（`query_map` 的映射函数只能返回这个）。
fn parse_kind_row(raw: &str) -> rusqlite::Result<MemoryKind> {
    MemoryKind::parse(raw).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// 行 → [`Memory`]。`memory_type` 解析失败会中断整次查询：种类列有 CHECK 约束，
/// 走到这里还解析不出来，说明表被绕过本程序改过，宁可报错也不要静默当成某一类。
fn map_memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let raw_type: String = row.get(2)?;
    Ok(Memory {
        id: row.get(0)?,
        content: row.get(1)?,
        memory_type: parse_kind_row(&raw_type)?,
        expires_at: row.get(3)?,
        message_id: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub fn search_similar(
    db: &Database,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<(Memory, f32)>> {
    let bytes = to_byte_array(query_embedding);
    // 只召回未过期的记忆（trait / preference 无到期日，永不失效）；纯按相关度排序。
    // 注意：vec0 的 kNN 查询必须用 `k = ?` 约束，LIMIT 属于外层查询，
    // 否则 vec0 会报 "A LIMIT or 'k = ?' constraint is required"。
    //
    // 一次 JOIN 把整行 memories 带出来：早先这里是"查出 id 再逐条 get()"，每轮召回
    // 多打 limit 次主键查询（recall 取 5 条 = 5 次），这层往返是纯浪费。
    let mut stmt = db.conn().prepare(
        "SELECT m.id, m.content, m.memory_type, m.expires_at, m.message_id,
                m.created_at, m.updated_at, v.distance
         FROM memory_vectors v
         JOIN memories m ON m.id = v.memory_id
         WHERE v.embedding MATCH ?1
           AND k = ?3
           AND (m.expires_at IS NULL OR m.expires_at > ?2)
         ORDER BY v.distance",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![bytes.as_slice(), Utc::now().to_rfc3339(), limit as i64],
        |row| {
            Ok((
                Memory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    memory_type: parse_kind_row(&row.get::<_, String>(2)?)?,
                    expires_at: row.get(3)?,
                    message_id: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                },
                row.get::<_, f32>(7)?,
            ))
        },
    )?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn to_byte_array(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for v in data {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod memory_tests;
