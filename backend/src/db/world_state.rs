//! `world_state` 表（AI 自我状态单行）的持久化。
//!
//! 行数据见 [`WorldStateRow`]；业务层 [`crate::world::self_state`] 在此之上做
//! 默认档案种子化、情绪衰减等逻辑，并与本模块互相映射。

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::db::Database;
use crate::error::Result;

/// `world_state` 单行数据（AI 自我档案 + 情绪轴 + 今日叙事 + 更新时间）。
#[derive(Debug, Clone)]
pub struct WorldStateRow {
    pub self_base: String,
    pub valence: f32,
    pub energy: f32,
    pub today_date: Option<String>,
    pub today_narrative: String,
    pub last_phase: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

pub fn load(db: &Database) -> Result<Option<WorldStateRow>> {
    let mut stmt = db.conn().prepare(
        "SELECT self_base, valence, energy, today_date, today_narrative, last_phase, updated_at
         FROM world_state WHERE id = 1",
    )?;
    let mut rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f32>(1)?,
            r.get::<_, f32>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    match rows.next() {
        Some(Ok((base, val, eng, date, narr, phase, upd))) => Ok(Some(WorldStateRow {
            self_base: base,
            valence: val,
            energy: eng,
            today_date: date,
            today_narrative: narr,
            last_phase: phase,
            updated_at: parse_ts(upd),
        })),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// 写回单行（`ON CONFLICT(id)` upsert，绝不产生第二行）。
pub fn save(db: &Database, row: &WorldStateRow) -> Result<()> {
    db.conn().execute(
        "INSERT INTO world_state
         (id, self_base, valence, energy, today_date, today_narrative, last_phase, updated_at)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
           self_base       = excluded.self_base,
           valence         = excluded.valence,
           energy          = excluded.energy,
           today_date      = excluded.today_date,
           today_narrative = excluded.today_narrative,
           last_phase      = excluded.last_phase,
           updated_at      = excluded.updated_at",
        params![
            row.self_base,
            row.valence,
            row.energy,
            row.today_date,
            row.today_narrative,
            row.last_phase,
            fmt(row.updated_at),
        ],
    )?;
    Ok(())
}

fn fmt(t: Option<DateTime<Utc>>) -> String {
    t.unwrap_or_else(Utc::now).to_rfc3339()
}

fn parse_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn save_updates_single_world_state_row() {
        let db = Database::in_memory().unwrap();
        assert!(load(&db).unwrap().is_none(), "未种子化的新库应无行");

        let row = WorldStateRow {
            self_base: "base".to_string(),
            valence: 0.3,
            energy: 0.33,
            today_date: Some("2026-09-17".to_string()),
            today_narrative: "narr".to_string(),
            last_phase: Some("上午".to_string()),
            updated_at: Some(Utc::now()),
        };
        save(&db, &row).unwrap();

        let got = load(&db).unwrap().unwrap();
        assert_eq!(got.self_base, "base");
        assert_eq!(got.today_narrative, "narr");
        assert!((got.valence - 0.3).abs() < 1e-4);

        // 再次写回应 upsert 成同一行，而不是追加第二行。
        save(&db, &row).unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM world_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}