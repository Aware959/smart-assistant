//! `relations` 表（按用户的亲密度 × 信任度）的持久化。
//!
//! 事件升温 / 衰减等业务逻辑在 [`crate::world::relation`]，本模块只负责行存取。

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;

/// `relations` 行数据。
#[derive(Debug, Clone)]
pub struct RelationRow {
    pub channel: String,
    pub external_id: String,
    pub closeness: f32,
    pub trust: f32,
    pub updated_at: DateTime<Utc>,
}

pub fn load(db: &Database, channel: &str, external_id: &str) -> Result<Option<RelationRow>> {
    let mut stmt = db.conn().prepare(
        "SELECT closeness, trust, updated_at
         FROM relations WHERE channel = ?1 AND external_id = ?2",
    )?;
    let mut rows = stmt.query_map(params![channel, external_id], |r| {
        Ok((
            r.get::<_, f32>(0)?,
            r.get::<_, f32>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    match rows.next() {
        Some(Ok((c, t, upd))) => match parse_ts(upd) {
            Some(updated_at) => Ok(Some(RelationRow {
                channel: channel.to_string(),
                external_id: external_id.to_string(),
                closeness: c,
                trust: t,
                updated_at,
            })),
            None => Ok(None),
        },
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn save(db: &Database, row: &RelationRow) -> Result<()> {
    db.conn().execute(
        "INSERT INTO relations (channel, external_id, closeness, trust, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(channel, external_id) DO UPDATE SET
           closeness  = excluded.closeness,
           trust      = excluded.trust,
           updated_at = excluded.updated_at",
        params![
            row.channel,
            row.external_id,
            row.closeness.clamp(0.0, 1.0),
            row.trust.clamp(0.0, 1.0),
            row.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// 读全表（供心跳对每条关系做时间衰减）。
pub fn list_all(db: &Database) -> Result<Vec<RelationRow>> {
    let mut stmt = db.conn().prepare(
        "SELECT channel, external_id, closeness, trust, updated_at FROM relations",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f32>(2)?,
            r.get::<_, f32>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ch, ext, c, t, upd) = row?;
        if let Some(updated_at) = parse_ts(upd) {
            out.push(RelationRow {
                channel: ch,
                external_id: ext,
                closeness: c,
                trust: t,
                updated_at,
            });
        }
    }
    Ok(out)
}

fn parse_ts(s: String) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&s).ok().map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn save_load_and_list_roundtrip() {
        let db = Database::in_memory().unwrap();
        assert!(load(&db, "telegram", "100").unwrap().is_none());

        let row = RelationRow {
            channel: "telegram".to_string(),
            external_id: "100".to_string(),
            closeness: 0.3,
            trust: 0.2,
            updated_at: Utc::now(),
        };
        save(&db, &row).unwrap();

        let got = load(&db, "telegram", "100").unwrap().unwrap();
        assert_eq!(got.external_id, "100");
        assert!((got.closeness - 0.3).abs() < 1e-4);
        assert_eq!(list_all(&db).unwrap().len(), 1);

        // upsert 同一用户：仍是一行。
        save(&db, &row).unwrap();
        assert_eq!(list_all(&db).unwrap().len(), 1);
    }
}