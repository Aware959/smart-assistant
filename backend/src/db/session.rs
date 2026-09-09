use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    pub fn new(title: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

pub fn create(db: &Database, title: &str) -> Result<Session> {
    let session = Session::new(title.to_string());
    db.conn().execute(
        "INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            session.id,
            session.title,
            session.created_at,
            session.updated_at
        ],
    )?;
    Ok(session)
}

pub fn get(db: &Database, id: &str) -> Result<Option<Session>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, title, created_at, updated_at FROM sessions WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |row| {
        Ok(Session {
            id: row.get(0)?,
            title: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
        })
    })?;
    if let Some(row) = rows.next() {
        Ok(Some(row?))
    } else {
        Ok(None)
    }
}

pub fn touch(db: &Database, id: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![Utc::now().to_rfc3339(), id],
    )?;
    Ok(())
}

pub fn update_title(db: &Database, id: &str, title: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![title, Utc::now().to_rfc3339(), id],
    )?;
    Ok(())
}

pub fn list(db: &Database, limit: usize) -> Result<Vec<Session>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, title, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok(Session {
            id: row.get(0)?,
            title: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn delete(db: &Database, id: &str) -> Result<()> {
    db.conn().execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    Ok(())
}