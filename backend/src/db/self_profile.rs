//! AI 自我属性 KV 表读写：人设数据源（性格/三观/爱好…）。
//!
//! 属性量少、全量常驻直读：注入提示词时整表读出按插入序渲染。
//! `set` / `delete` 目前没有业务接线（v1 人工维护），保留为 aiworld 重设计后
//! AI 自我演化的写入口。

use chrono::Utc;
use rusqlite::{params, Connection};

use crate::error::Result;

/// 读取全部属性（按插入序，key 在前 value 在后）。
pub(crate) fn all(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM self_profile ORDER BY rowid")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

/// 写入 / 覆盖属性（刷新 updated_at）。
///
/// v1 无业务调用（人工维护），为 aiworld 重设计后 AI 自我演化预留。
#[allow(dead_code)]
pub(crate) fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO self_profile (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// 删除属性（键不存在则为 no-op）。
///
/// v1 无业务调用（人工维护），为 aiworld 重设计后 AI 自我演化预留。
#[allow(dead_code)]
pub(crate) fn delete(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM self_profile WHERE key = ?1", [key])?;
    Ok(())
}

#[cfg(test)]
#[path = "self_profile_tests.rs"]
mod self_profile_tests;