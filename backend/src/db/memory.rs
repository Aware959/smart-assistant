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
    /// 来源消息 id（对话中被沉淀的输入），无则说明是手动添加。
    pub message_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Memory {
    pub fn new(content: String, memory_type: String, message_id: Option<String>) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            memory_type,
            message_id,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

pub fn create(db: &Database, memory: &Memory, embedding: &[f32]) -> Result<()> {
    db.conn().execute(
        "INSERT INTO memories (id, content, memory_type, message_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            memory.id,
            memory.content,
            memory.memory_type,
            memory.message_id,
            memory.created_at,
            memory.updated_at
        ],
    )?;

    let bytes = to_byte_array(embedding);
    db.conn().execute(
        "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![memory.id, bytes.as_slice()],
    )?;

    Ok(())
}

pub fn get(db: &Database, id: &str) -> Result<Memory> {
    let row = db.conn().query_row(
        "SELECT id, content, memory_type, message_id, created_at, updated_at
         FROM memories WHERE id = ?1",
        [id],
        |row| {
            Ok(Memory {
                id: row.get(0)?,
                content: row.get(1)?,
                memory_type: row.get(2)?,
                message_id: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )?;
    Ok(row)
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
    // 该记忆派生实体的参与关系，先于实体显式删除（不依赖外键级联）。
    tx.execute(
        "DELETE FROM relations
         WHERE source_id IN (SELECT id FROM entities WHERE memory_id = ?1)
            OR target_id IN (SELECT id FROM entities WHERE memory_id = ?1)",
        [id],
    )?;
    tx.execute("DELETE FROM entities WHERE memory_id = ?1", [id])?;
    tx.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [id])?;
    tx.execute("DELETE FROM memories WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn list(db: &Database, limit: usize) -> Result<Vec<Memory>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, content, memory_type, message_id, created_at, updated_at
         FROM memories ORDER BY created_at DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit as i64], |row| {
        Ok(Memory {
            id: row.get(0)?,
            content: row.get(1)?,
            memory_type: row.get(2)?,
            message_id: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn search_similar(
    db: &Database,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<(Memory, f32)>> {
    let bytes = to_byte_array(query_embedding);
    let mut stmt = db.conn().prepare(
        "SELECT v.memory_id, v.distance
         FROM memory_vectors v
         WHERE v.embedding MATCH ?1
         ORDER BY v.distance
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![bytes.as_slice(), limit as i64],
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
