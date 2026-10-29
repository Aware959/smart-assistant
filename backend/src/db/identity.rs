//! 身份变更提案与版本审计的 SQL 适配器。
//!
//! 只负责持久化形状，不判断字段是否合法、状态转换是否允许，也不直接改
//! `self_profile`。这些业务规则属于身份服务：提案生成、审批、事务应用与回滚都在
//! 服务层完成，避免任意调用方绕过状态机直接写表。
//!
//! I2 只建立持久化边界，接口尚未接入服务层；`allow(dead_code)` 是给下一阶段
//! 版本化渲染与审批服务预留的，不是长期死代码。

#![allow(dead_code)]

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::error::{Result, SqlError};

/// 一条身份变更提案。
///
/// `before_value` 允许为空：新增一个此前未设置的字段时没有 before。`base_version`
/// 是提案生成时读到的身份版本，审批应用时据此做乐观并发检查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityChangeProposal {
    pub id: String,
    pub field_key: String,
    pub before_value: Option<String>,
    pub after_value: String,
    pub reason: String,
    /// JSON 数组或对象；证据格式由身份服务定义，DB 只保证非空可追溯。
    pub evidence: String,
    pub confidence: String,
    pub risk_level: String,
    pub status: String,
    pub proposer: String,
    pub base_version: i64,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub applied_at: Option<DateTime<Utc>>,
    pub rolled_back_at: Option<DateTime<Utc>>,
}

/// 一条已生效身份快照的审计记录。
///
/// `snapshot` 是完整身份快照的 JSON；`proposal_id` 为空表示人工直接写入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityVersion {
    pub version: i64,
    pub snapshot: String,
    pub proposal_id: Option<String>,
    pub source: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub rolled_back_from: Option<i64>,
}

const PROPOSAL_COLUMNS: &str = "id, field_key, before_value, after_value, reason, evidence, \
     confidence, risk_level, status, proposer, base_version, created_at, decided_at, \
     applied_at, rolled_back_at";

const VERSION_COLUMNS: &str =
    "version, snapshot, proposal_id, source, reason, created_at, rolled_back_from";

/// 插入一条提案。调用方负责先校验字段、状态与内容。
pub(crate) fn insert_proposal(db: &Database, p: &IdentityChangeProposal) -> Result<()> {
    db.conn().execute(
        "INSERT INTO identity_change_proposals \
         (id, field_key, before_value, after_value, reason, evidence, confidence, risk_level, \
          status, proposer, base_version, created_at, decided_at, applied_at, rolled_back_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            p.id,
            p.field_key,
            p.before_value,
            p.after_value,
            p.reason,
            p.evidence,
            p.confidence,
            p.risk_level,
            p.status,
            p.proposer,
            p.base_version,
            p.created_at.to_rfc3339(),
            p.decided_at.map(|t| t.to_rfc3339()),
            p.applied_at.map(|t| t.to_rfc3339()),
            p.rolled_back_at.map(|t| t.to_rfc3339()),
        ],
    )?;
    Ok(())
}

pub(crate) fn get_proposal(db: &Database, id: &str) -> Result<Option<IdentityChangeProposal>> {
    let sql = format!("SELECT {PROPOSAL_COLUMNS} FROM identity_change_proposals WHERE id = ?1");
    let mut stmt = db.conn().prepare(&sql)?;
    let mut rows = stmt.query_map([id], map_proposal)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// 按创建时刻倒序列出指定状态的提案。
pub(crate) fn list_proposals_by_status(
    db: &Database,
    status: &str,
    limit: usize,
) -> Result<Vec<IdentityChangeProposal>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {PROPOSAL_COLUMNS} FROM identity_change_proposals \
         WHERE status = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2"
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![status, limit as i64], map_proposal)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// 只更新审批决定（pending → approved / rejected），不动应用时间。
pub(crate) fn decide_proposal(
    db: &Database,
    id: &str,
    status: &str,
    decided_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = db.conn().execute(
        "UPDATE identity_change_proposals SET status = ?1, decided_at = ?2 \
         WHERE id = ?3 AND status = 'pending'",
        rusqlite::params![status, decided_at.to_rfc3339(), id],
    )?;
    Ok(changed == 1)
}

/// 标记提案已生效。只允许 approved → applied，防止未审批提案被直接应用。
pub(crate) fn mark_proposal_applied(
    db: &Database,
    id: &str,
    applied_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = db.conn().execute(
        "UPDATE identity_change_proposals SET status = 'applied', applied_at = ?1 \
         WHERE id = ?2 AND status = 'approved'",
        rusqlite::params![applied_at.to_rfc3339(), id],
    )?;
    Ok(changed == 1)
}

/// 标记提案已回滚。只允许 applied → rolled_back。
pub(crate) fn mark_proposal_rolled_back(
    db: &Database,
    id: &str,
    rolled_back_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = db.conn().execute(
        "UPDATE identity_change_proposals SET status = 'rolled_back', rolled_back_at = ?1 \
         WHERE id = ?2 AND status = 'applied'",
        rusqlite::params![rolled_back_at.to_rfc3339(), id],
    )?;
    Ok(changed == 1)
}

/// 插入一条身份版本审计记录。版本号必须由服务层在同一事务中计算。
pub(crate) fn insert_version(db: &Database, v: &IdentityVersion) -> Result<()> {
    db.conn().execute(
        "INSERT INTO self_profile_versions \
         (version, snapshot, proposal_id, source, reason, created_at, rolled_back_from) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            v.version,
            v.snapshot,
            v.proposal_id,
            v.source,
            v.reason,
            v.created_at.to_rfc3339(),
            v.rolled_back_from,
        ],
    )?;
    Ok(())
}

pub(crate) fn get_version(db: &Database, version: i64) -> Result<Option<IdentityVersion>> {
    let sql = format!("SELECT {VERSION_COLUMNS} FROM self_profile_versions WHERE version = ?1");
    let mut stmt = db.conn().prepare(&sql)?;
    let mut rows = stmt.query_map([version], map_version)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// 最新版本记录；没有版本时返回 None。
pub(crate) fn latest_version(db: &Database) -> Result<Option<IdentityVersion>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS} FROM self_profile_versions ORDER BY version DESC LIMIT 1"
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let mut rows = stmt.query_map([], map_version)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// 按版本倒序列出审计记录。
pub(crate) fn list_versions(db: &Database, limit: usize) -> Result<Vec<IdentityVersion>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {VERSION_COLUMNS} FROM self_profile_versions \
         ORDER BY version DESC LIMIT ?1"
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], map_version)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn map_proposal(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdentityChangeProposal> {
    Ok(IdentityChangeProposal {
        id: row.get(0)?,
        field_key: row.get(1)?,
        before_value: row.get(2)?,
        after_value: row.get(3)?,
        reason: row.get(4)?,
        evidence: row.get(5)?,
        confidence: row.get(6)?,
        risk_level: row.get(7)?,
        status: row.get(8)?,
        proposer: row.get(9)?,
        base_version: row.get(10)?,
        created_at: parse_utc(row.get::<_, String>(11)?)?,
        decided_at: row
            .get::<_, Option<String>>(12)?
            .map(parse_utc)
            .transpose()?,
        applied_at: row
            .get::<_, Option<String>>(13)?
            .map(parse_utc)
            .transpose()?,
        rolled_back_at: row
            .get::<_, Option<String>>(14)?
            .map(parse_utc)
            .transpose()?,
    })
}

fn map_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdentityVersion> {
    Ok(IdentityVersion {
        version: row.get(0)?,
        snapshot: row.get(1)?,
        proposal_id: row.get(2)?,
        source: row.get(3)?,
        reason: row.get(4)?,
        created_at: parse_utc(row.get::<_, String>(5)?)?,
        rolled_back_from: row.get(6)?,
    })
}

fn parse_utc(s: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

/// 供后续服务层使用：把 SQL 层的“未找到”统一成错误。
pub(crate) fn proposal_or_err(db: &Database, id: &str) -> Result<IdentityChangeProposal> {
    get_proposal(db, id)?.ok_or_else(|| SqlError::NotFound(format!("identity proposal {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(secs)
    }

    fn proposal(id: &str, status: &str) -> IdentityChangeProposal {
        IdentityChangeProposal {
            id: id.to_string(),
            field_key: "voice_style".to_string(),
            before_value: Some("更简短".to_string()),
            after_value: "更愿意主动分享自己的看法，但仍然保持短句".to_string(),
            reason: "用户多次表示喜欢有观点的回复".to_string(),
            evidence: r#"["m-1","m-2"]"#.to_string(),
            confidence: "medium".to_string(),
            risk_level: "low".to_string(),
            status: status.to_string(),
            proposer: "identity-evolution-v1".to_string(),
            base_version: 1,
            created_at: at(0),
            decided_at: None,
            applied_at: None,
            rolled_back_at: None,
        }
    }

    fn version(version: i64) -> IdentityVersion {
        IdentityVersion {
            version,
            snapshot: format!(r#"{{"voice_style":"v{version}"}}"#),
            proposal_id: Some(format!("p{version}")),
            source: "evolution_agent".to_string(),
            reason: "测试".to_string(),
            created_at: at(version),
            rolled_back_from: None,
        }
    }

    #[test]
    fn proposal_roundtrips_with_all_optional_times() {
        let db = Database::in_memory().unwrap();
        let mut p = proposal("p1", "pending");
        p.decided_at = Some(at(10));
        p.applied_at = Some(at(20));
        p.rolled_back_at = Some(at(30));
        insert_proposal(&db, &p).unwrap();

        assert_eq!(get_proposal(&db, "p1").unwrap(), Some(p));
        assert_eq!(get_proposal(&db, "missing").unwrap(), None);
    }

    #[test]
    fn proposal_list_filters_status_and_honors_limit() {
        let db = Database::in_memory().unwrap();
        insert_proposal(&db, &proposal("old", "pending")).unwrap();
        insert_proposal(&db, &proposal("new", "pending")).unwrap();
        insert_proposal(&db, &proposal("applied", "applied")).unwrap();

        let pending = list_proposals_by_status(&db, "pending", 10).unwrap();
        assert_eq!(
            pending.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["new", "old"],
            "同状态应按创建时刻倒序"
        );
        assert_eq!(
            list_proposals_by_status(&db, "pending", 1).unwrap().len(),
            1
        );
        assert!(list_proposals_by_status(&db, "pending", 0)
            .unwrap()
            .is_empty());
        assert!(list_proposals_by_status(&db, "unknown", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn proposal_state_updates_only_from_expected_previous_state() {
        let db = Database::in_memory().unwrap();
        insert_proposal(&db, &proposal("p1", "pending")).unwrap();

        assert!(decide_proposal(&db, "p1", "approved", at(1)).unwrap());
        assert!(
            !decide_proposal(&db, "p1", "rejected", at(2)).unwrap(),
            "approved 不能再被 rejected"
        );
        assert_eq!(get_proposal(&db, "p1").unwrap().unwrap().status, "approved");
    }

    #[test]
    fn proposal_can_only_apply_after_approval_and_rollback_after_apply() {
        let db = Database::in_memory().unwrap();
        insert_proposal(&db, &proposal("p1", "pending")).unwrap();

        assert!(!mark_proposal_applied(&db, "p1", at(1)).unwrap());
        assert!(decide_proposal(&db, "p1", "approved", at(2)).unwrap());
        assert!(mark_proposal_applied(&db, "p1", at(3)).unwrap());
        assert!(mark_proposal_rolled_back(&db, "p1", at(4)).unwrap());
        assert!(
            !mark_proposal_rolled_back(&db, "p1", at(5)).unwrap(),
            "同一提案不能重复回滚"
        );

        let p = get_proposal(&db, "p1").unwrap().unwrap();
        assert_eq!(p.status, "rolled_back");
        assert_eq!(p.applied_at, Some(at(3)));
        assert_eq!(p.rolled_back_at, Some(at(4)));
    }

    #[test]
    fn version_roundtrips_and_lists_newest_first() {
        let db = Database::in_memory().unwrap();
        insert_version(&db, &version(1)).unwrap();
        insert_version(&db, &version(2)).unwrap();

        assert_eq!(get_version(&db, 1).unwrap(), Some(version(1)));
        assert_eq!(latest_version(&db).unwrap(), Some(version(2)));
        assert_eq!(
            list_versions(&db, 10)
                .unwrap()
                .iter()
                .map(|v| v.version)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(list_versions(&db, 0).unwrap().is_empty());
    }

    #[test]
    fn duplicate_version_is_rejected_by_primary_key() {
        let db = Database::in_memory().unwrap();
        insert_version(&db, &version(1)).unwrap();
        assert!(insert_version(&db, &version(1)).is_err());
    }

    #[test]
    fn proposal_or_err_reports_missing_id() {
        let db = Database::in_memory().unwrap();
        assert!(matches!(
            proposal_or_err(&db, "missing"),
            Err(SqlError::NotFound(_))
        ));
    }
}
