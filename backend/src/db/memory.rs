use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{Result, SqlError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    /// 记忆层级：short（短期/易过期）| intent（意向/计划）| core（长期强事实）。
    pub tier: String,
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
        memory_type: String,
        tier: String,
        message_id: Option<String>,
        expires_at: Option<String>,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            memory_type,
            tier,
            expires_at,
            message_id,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

pub fn create_record(db: &Database, memory: &Memory) -> Result<()> {
    db.conn().execute(
        "INSERT INTO memories (id, content, memory_type, tier, expires_at, message_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            memory.id,
            memory.content,
            memory.memory_type,
            memory.tier,
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
        "SELECT id, content, memory_type, tier, expires_at, message_id, created_at, updated_at
         FROM memories WHERE id = ?1",
        [id],
        |row| {
            Ok(Memory {
                id: row.get(0)?,
                content: row.get(1)?,
                memory_type: row.get(2)?,
                tier: row.get(3)?,
                expires_at: row.get(4)?,
                message_id: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        },
    )?;
    Ok(row)
}

/// 只刷新 updated_at（用于去重命中时"续期"而不新增）。
pub fn touch(db: &Database, id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    db.conn().execute(
        "UPDATE memories SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

pub fn update(db: &Database, id: &str, new_content: &str, new_embedding: &[f32]) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    if new_embedding.is_empty() {
        db.conn().execute(
            "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![new_content, now, id],
        )?;
    } else {
        db.conn().execute(
            "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![new_content, now, id],
        )?;

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
        "SELECT id, content, memory_type, tier, expires_at, message_id, created_at, updated_at
         FROM memories ORDER BY created_at DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit as i64], map_memory_row)?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

fn map_memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get(0)?,
        content: row.get(1)?,
        memory_type: row.get(2)?,
        tier: row.get(3)?,
        expires_at: row.get(4)?,
        message_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

pub fn search_similar(
    db: &Database,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<(Memory, f32)>> {
    let bytes = to_byte_array(query_embedding);
    // 只召回未过期的记忆（core 永不失效）；纯按相关度排序。
    // 注意：vec0 的 kNN 查询必须用 `k = ?` 约束，LIMIT 属于外层查询，
    // 否则 vec0 会报 "A LIMIT or 'k = ?' constraint is required"。
    let mut stmt = db.conn().prepare(
        "SELECT v.memory_id, v.distance
         FROM memory_vectors v
         JOIN memories m ON m.id = v.memory_id
         WHERE v.embedding MATCH ?1
           AND k = ?3
           AND (m.expires_at IS NULL OR m.expires_at > ?2)
         ORDER BY v.distance",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![bytes.as_slice(), Utc::now().to_rfc3339(), limit as i64],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?)),
    )?;

    let mut result = Vec::new();
    for row in rows {
        let (id, distance) = row?;
        if let Ok(memory) = get(db, &id) {
            result.push((memory, distance));
        }
    }
    Ok(result)
}

pub fn to_byte_array(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for v in data {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

pub fn from_byte_array(bytes: &[u8]) -> Result<Vec<f32>> {
    let mut data = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        data.push(f32::from_le_bytes(
            chunk.try_into().map_err(|_| SqlError::Config("invalid f32".into()))?,
        ));
    }
    Ok(data)
}
