use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub memory_id: Option<String>,
    pub created_at: String,
}

impl Entity {
    pub fn new(name: String, entity_type: String, memory_id: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            entity_type,
            memory_id,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation_type: String,
    pub weight: f32,
    pub memory_id: Option<String>,
    pub created_at: String,
}

impl Relation {
    pub fn new(
        source_id: String,
        target_id: String,
        relation_type: String,
        weight: f32,
        memory_id: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            source_id,
            target_id,
            relation_type,
            weight,
            memory_id,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

/// 确保实体存在（按名称去重），返回实体 ID。
///
/// 新建实体时记录其归属记忆 `memory_id`（用于删除记忆时级联清理）；
/// 已存在的实体只复用，不改变归属。
pub fn ensure_entity(
    db: &Database,
    name: &str,
    entity_type: &str,
    memory_id: Option<&str>,
) -> Result<String> {
    let existing = db.conn().query_row(
        "SELECT id FROM entities WHERE name = ?1 LIMIT 1",
        [name],
        |row| row.get::<_, String>(0),
    );

    if let Ok(id) = existing {
        return Ok(id);
    }

    let entity = Entity::new(name.to_string(), entity_type.to_string(), memory_id.map(String::from));
    db.conn().execute(
        "INSERT INTO entities (id, name, entity_type, memory_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            entity.id,
            entity.name,
            entity.entity_type,
            entity.memory_id,
            entity.created_at
        ],
    )?;
    Ok(entity.id)
}

pub fn upsert_relation(
    db: &Database,
    source_id: &str,
    target_id: &str,
    relation_type: &str,
    weight: f32,
    memory_id: Option<&str>,
) -> Result<()> {
    let existing = db.conn().query_row(
        "SELECT id FROM relations
         WHERE source_id = ?1 AND target_id = ?2 AND relation_type = ?3
         LIMIT 1",
        params![source_id, target_id, relation_type],
        |row| row.get::<_, String>(0),
    );

    if let Ok(id) = existing {
        let new_weight = db.conn().query_row(
            "SELECT COALESCE(weight, 0) FROM relations WHERE id = ?1",
            [&id],
            |row| row.get::<_, f32>(0),
        )? + weight;
        db.conn().execute(
            "UPDATE relations SET weight = ?1 WHERE id = ?2",
            params![new_weight, id],
        )?;
    } else {
        let relation = Relation::new(
            source_id.to_string(),
            target_id.to_string(),
            relation_type.to_string(),
            weight,
            memory_id.map(|s| s.to_string()),
        );
        db.conn().execute(
            "INSERT INTO relations
             (id, source_id, target_id, relation_type, weight, memory_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                relation.id,
                relation.source_id,
                relation.target_id,
                relation.relation_type,
                relation.weight,
                relation.memory_id,
                relation.created_at
            ],
        )?;
    }
    Ok(())
}

/// 只读查询实体 ID（不会像 ensure_entity 那样创建新实体）。
pub fn find_entity_by_name(db: &Database, name: &str) -> Result<Option<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, name, entity_type, memory_id, created_at FROM entities WHERE name = ?1 LIMIT 1",
    )?;
    let mut rows = stmt.query_map([name], |row| {
        Ok(Entity {
            id: row.get(0)?,
            name: row.get(1)?,
            entity_type: row.get(2)?,
            memory_id: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;

    if let Some(row) = rows.next() {
        Ok(Some(row?))
    } else {
        Ok(None)
    }
}

/// 查询与某实体直接相连的所有关系（出边 + 入边），按权重降序。
pub fn list_relations_for_entity(db: &Database, entity_id: &str) -> Result<Vec<Relation>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, source_id, target_id, relation_type, weight, memory_id, created_at
         FROM relations
         WHERE source_id = ?1 OR target_id = ?1
         ORDER BY weight DESC",
    )?;
    let rows = stmt.query_map([entity_id], |row| {
        Ok(Relation {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            relation_type: row.get(3)?,
            weight: row.get(4)?,
            memory_id: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn get_entity(db: &Database, id: &str) -> Result<Option<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, name, entity_type, memory_id, created_at FROM entities WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |row| {
        Ok(Entity {
            id: row.get(0)?,
            name: row.get(1)?,
            entity_type: row.get(2)?,
            memory_id: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;

    if let Some(row) = rows.next() {
        Ok(Some(row?))
    } else {
        Ok(None)
    }
}

pub fn list_entities(db: &Database) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, name, entity_type, memory_id, created_at FROM entities
         ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Entity {
            id: row.get(0)?,
            name: row.get(1)?,
            entity_type: row.get(2)?,
            memory_id: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn list_relations(db: &Database) -> Result<Vec<Relation>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, source_id, target_id, relation_type, weight, memory_id, created_at
         FROM relations ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Relation {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            relation_type: row.get(3)?,
            weight: row.get(4)?,
            memory_id: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn list_relations_by_type(db: &Database, relation_type: &str) -> Result<Vec<Relation>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, source_id, target_id, relation_type, weight, memory_id, created_at
         FROM relations WHERE relation_type = ?1 ORDER BY weight DESC",
    )?;
    let rows = stmt.query_map([relation_type], |row| {
        Ok(Relation {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            relation_type: row.get(3)?,
            weight: row.get(4)?,
            memory_id: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn delete_relation(db: &Database, id: &str) -> Result<()> {
    db.conn().execute("DELETE FROM relations WHERE id = ?1", [id])?;
    Ok(())
}

/// 删除一个实体，并同步删除该实体参与的**全部关系**（出边 + 入边）。
///
/// 显式删除而不依赖 SQLite 外键级联：单事务内先清理关系、再删实体，
/// 保证无论外键配置如何，"删实体 → 对应关系一并消失" 语义都成立。
pub fn delete_entity(db: &Database, id: &str) -> Result<()> {
    let tx = db.conn().unchecked_transaction()?;
    tx.execute(
        "DELETE FROM relations WHERE source_id = ?1 OR target_id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM entities WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}
