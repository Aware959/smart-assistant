//! 身份变更提案、事务应用与版本审计的 SQL 适配器。
//!
//! 提案创建与人工审批的输入校验在身份服务完成。本模块负责把审批后的变更作为
//! 单个 SQLite 事务落库：更新 `self_profile` canonical key、追加完整身份快照、
//! 推进提案状态；任何一步失败都由事务整体回滚。

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::core::identity::{
    AgentIdentity, ChangeSource, IdentityError, IdentityFieldKey, IdentityFieldValue,
    ProposalStatus,
};
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
    pub decided_by: Option<String>,
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
    pub operator: Option<String>,
    pub rolled_back_from: Option<i64>,
}

const PROPOSAL_COLUMNS: &str = "id, field_key, before_value, after_value, reason, evidence, \
     confidence, risk_level, status, proposer, base_version, created_at, decided_at, \
     decided_by, applied_at, rolled_back_at";

const VERSION_COLUMNS: &str =
    "version, snapshot, proposal_id, source, reason, created_at, operator, rolled_back_from";

/// 插入一条提案。调用方负责先校验字段、状态与内容。
pub(crate) fn insert_proposal(db: &Database, p: &IdentityChangeProposal) -> Result<()> {
    db.conn().execute(
        "INSERT INTO identity_change_proposals \
         (id, field_key, before_value, after_value, reason, evidence, confidence, risk_level, \
          status, proposer, base_version, created_at, decided_at, decided_by, applied_at, \
          rolled_back_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
            p.decided_by,
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

/// 指定字段在 `(since, until]` 内是否已有提案（不论状态）。
///
/// 身份服务用这个查询执行字段级冷却，避免被拒绝的提案不计入频率限制。
pub(crate) fn has_proposal_for_field_between(
    db: &Database,
    field_key: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<bool> {
    if since > until {
        return Ok(false);
    }
    let exists: i64 = db.conn().query_row(
        "SELECT EXISTS(
            SELECT 1 FROM identity_change_proposals
            WHERE field_key = ?1 AND created_at > ?2 AND created_at <= ?3
        )",
        rusqlite::params![field_key, since.to_rfc3339(), until.to_rfc3339()],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// 任意字段在 `(since, until]` 内是否已有提案（不论状态）。
///
/// 后台身份演化是低频反思，不按字段去重即可做到“每天最多一次”；与字段级 7 天
/// 冷却互补。
pub(crate) fn has_any_proposal_between(
    db: &Database,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<bool> {
    if since > until {
        return Ok(false);
    }
    let exists: i64 = db.conn().query_row(
        "SELECT EXISTS(
            SELECT 1 FROM identity_change_proposals
            WHERE created_at > ?1 AND created_at <= ?2
        )",
        params![since.to_rfc3339(), until.to_rfc3339()],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// 最近一次身份演化到点检查时间；调度用 UTC 墙钟补算，不随进程重启重置。
pub(crate) fn latest_evolution_check(db: &Database) -> Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = db
        .conn()
        .query_row(
            "SELECT last_checked_at FROM identity_evolution_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.map(parse_utc).transpose()?)
}

/// 原子记录一次到点检查；无论生成提案、无提案还是失败，都避免停机后反复补跑。
pub(crate) fn mark_evolution_checked(db: &Database, now: DateTime<Utc>) -> Result<()> {
    let timestamp = now.to_rfc3339();
    db.conn().execute(
        "INSERT INTO identity_evolution_runs (id, last_checked_at, updated_at)
         VALUES (1, ?1, ?1)
         ON CONFLICT(id) DO UPDATE SET
             last_checked_at = excluded.last_checked_at,
             updated_at = excluded.updated_at",
        params![timestamp],
    )?;
    Ok(())
}

/// 只更新审批决定（pending → approved / rejected），不动应用时间。
pub(crate) fn decide_proposal(
    db: &Database,
    id: &str,
    status: &str,
    operator: &str,
    decided_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = db.conn().execute(
        "UPDATE identity_change_proposals SET status = ?1, decided_at = ?2, decided_by = ?3 \
         WHERE id = ?4 AND status = 'pending'",
        rusqlite::params![status, decided_at.to_rfc3339(), operator, id],
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
    mark_proposal_rolled_back_on(db.conn(), id, rolled_back_at)
}

/// 在同一事务内批准（若仍为 pending）并应用一条提案。
///
/// 允许 `pending` 或 `approved` 状态：`approve_and_apply` 需要把审批与生效合成一个
/// 原子操作；已通过 `decide` 审批的提案也可随后直接应用。应用前校验提案基线版本和
/// `before_value` 仍与当前身份一致，避免覆盖提案生成后的并发变更。
pub(crate) fn approve_and_apply_proposal(
    db: &Database,
    id: &str,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<IdentityVersion> {
    let tx = db.conn().unchecked_transaction()?;
    let proposal = proposal_on(&tx, id)?
        .ok_or_else(|| SqlError::NotFound(format!("identity proposal {id}")))?;
    let status = ProposalStatus::parse(&proposal.status).map_err(identity_error)?;
    if !matches!(status, ProposalStatus::Pending | ProposalStatus::Approved) {
        return Err(SqlError::Config(format!(
            "提案 {id} 状态为 {}，只有 pending / approved 可以应用",
            proposal.status
        )));
    }

    let field = IdentityFieldKey::parse(&proposal.field_key).map_err(identity_error)?;
    let after = IdentityFieldValue::new(field, &proposal.after_value).map_err(identity_error)?;
    let base = current_identity_on(&tx, now)?;
    ensure_base_version(&proposal, base.version())?;
    ensure_before_value(&proposal, field, &base)?;

    let mut fields = base.fields().clone();
    fields.insert(field, after.clone());
    let next_version = next_version_on(&tx)?;
    let applied = AgentIdentity::new(next_version, now, fields);
    let snapshot = applied.to_snapshot_json()?;

    if status == ProposalStatus::Pending {
        let changed =
            decide_proposal_on(&tx, id, ProposalStatus::Approved.as_str(), operator, now)?;
        if !changed {
            return Err(SqlError::Config(format!(
                "提案 {id} 审批状态已变化，不能应用"
            )));
        }
    }

    replace_profile_field_on(&tx, field, Some(after.as_str()), now)?;
    let version = IdentityVersion {
        version: i64::try_from(next_version)
            .map_err(|_| SqlError::Config(format!("身份版本 {next_version} 超出数据库范围")))?,
        snapshot,
        proposal_id: Some(proposal.id.clone()),
        source: ChangeSource::EvolutionAgent.as_str().to_string(),
        reason: proposal.reason.clone(),
        created_at: now,
        operator: Some(operator.to_string()),
        rolled_back_from: None,
    };
    insert_version_on(&tx, &version)?;
    if !mark_proposal_applied_on(&tx, id, now)? {
        return Err(SqlError::Config(format!(
            "提案 {id} 未能从 approved 转为 applied"
        )));
    }

    tx.commit()?;
    Ok(version)
}

/// 回滚当前最新身份版本所对应的 applied 提案。
///
/// 恢复该版本之前的上一条完整快照；若应用的是未版本化身份（版本 1），则按提案
/// `before_value` 逆变换得到原始快照。回滚本身追加一条 `rollback` 版本并把原提案
/// 标记为 `rolled_back`，不删除历史。
pub(crate) fn rollback_latest_applied(
    db: &Database,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<IdentityVersion> {
    let tx = db.conn().unchecked_transaction()?;
    let current = latest_version_on(&tx)?
        .ok_or_else(|| SqlError::Config("没有身份版本，无法回滚".to_string()))?;
    if current.source != ChangeSource::EvolutionAgent.as_str() {
        return Err(SqlError::Config(format!(
            "最新身份版本 {} 的来源是 {}，不是 evolution_agent，拒绝回滚",
            current.version, current.source
        )));
    }
    let proposal_id = current
        .proposal_id
        .as_deref()
        .ok_or_else(|| SqlError::Config(format!("最新身份版本 {} 缺少提案 id", current.version)))?;
    let proposal = proposal_on(&tx, proposal_id)?
        .ok_or_else(|| SqlError::NotFound(format!("identity proposal {proposal_id}")))?;
    if ProposalStatus::parse(&proposal.status).map_err(identity_error)? != ProposalStatus::Applied {
        return Err(SqlError::Config(format!(
            "最新身份版本对应的提案 {} 状态为 {}，不是 applied",
            proposal.id, proposal.status
        )));
    }

    let field = IdentityFieldKey::parse(&proposal.field_key).map_err(identity_error)?;
    let current_identity = AgentIdentity::from_snapshot_json(
        current.version_number()?,
        current.created_at,
        &current.snapshot,
    )
    .map_err(identity_error)?;
    let current_value = current_identity.get(field).map(IdentityFieldValue::as_str);
    if current_value != Some(proposal.after_value.as_str()) {
        return Err(SqlError::Config(format!(
            "提案 {} 的 after_value 与最新身份快照不一致，拒绝回滚",
            proposal.id
        )));
    }

    let restored_snapshot = match latest_version_before_on(&tx, current.version)? {
        Some(previous) => previous.snapshot,
        None if current.version == 1 => snapshot_before_applying(
            &current.snapshot,
            field,
            proposal.before_value.as_deref(),
            now,
        )?,
        None => {
            return Err(SqlError::Config(format!(
                "身份版本 {} 缺少可恢复的上一条快照",
                current.version
            )));
        }
    };
    let restored =
        AgentIdentity::from_snapshot_json(0, now, &restored_snapshot).map_err(identity_error)?;
    let restored_value = restored.get(field).map(IdentityFieldValue::as_str);
    replace_profile_field_on(&tx, field, restored_value, now)?;

    let next_version = current
        .version
        .checked_add(1)
        .ok_or_else(|| SqlError::Config("身份版本已到数据库上限".to_string()))?;
    let version = IdentityVersion {
        version: next_version,
        snapshot: restored_snapshot,
        proposal_id: Some(proposal.id.clone()),
        source: ChangeSource::Rollback.as_str().to_string(),
        reason: format!("回滚提案 {}：{}", proposal.id, proposal.reason),
        created_at: now,
        operator: Some(operator.to_string()),
        rolled_back_from: Some(current.version),
    };
    insert_version_on(&tx, &version)?;
    if !mark_proposal_rolled_back_on(&tx, &proposal.id, now)? {
        return Err(SqlError::Config(format!(
            "提案 {} 未能从 applied 转为 rolled_back",
            proposal.id
        )));
    }

    tx.commit()?;
    Ok(version)
}

/// 恢复到任意更早的已审计身份快照。
///
/// 目标版本必须早于当前最新版本。恢复动作追加新的 `rollback` 版本，并把目标之后
/// 所有仍为 `applied` 的演化提案标记为 `rolled_back`；历史版本和提案行都不删除。
/// 与“只撤销最新一次”的 [`rollback_latest_applied`] 相比，这里恢复的是完整快照，
/// 因而可以一次跨过多次生效变更。
pub(crate) fn rollback_to_version(
    db: &Database,
    target_version: i64,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<IdentityVersion> {
    if target_version < 1 {
        return Err(SqlError::Config(format!(
            "目标身份版本 {target_version} 不是正整数"
        )));
    }
    let tx = db.conn().unchecked_transaction()?;
    let current = latest_version_on(&tx)?
        .ok_or_else(|| SqlError::Config("没有身份版本，无法回滚".to_string()))?;
    if target_version >= current.version {
        return Err(SqlError::Config(format!(
            "目标版本 v{target_version} 必须早于当前版本 v{}",
            current.version
        )));
    }
    let target = get_version_on(&tx, target_version)?
        .ok_or_else(|| SqlError::NotFound(format!("identity version {target_version}")))?;
    let restored = AgentIdentity::from_snapshot_json(0, target.created_at, &target.snapshot)
        .map_err(identity_error)?;
    replace_profile_snapshot_on(&tx, &restored, now)?;

    let next_version = current
        .version
        .checked_add(1)
        .ok_or_else(|| SqlError::Config("身份版本已到数据库上限".to_string()))?;
    let version = IdentityVersion {
        version: next_version,
        snapshot: target.snapshot,
        proposal_id: None,
        source: ChangeSource::Rollback.as_str().to_string(),
        reason: format!("恢复到身份版本 v{target_version}"),
        created_at: now,
        operator: Some(operator.to_string()),
        rolled_back_from: Some(current.version),
    };
    insert_version_on(&tx, &version)?;
    mark_applied_versions_after_on(&tx, target_version, now)?;

    tx.commit()?;
    Ok(version)
}

/// 插入一条身份版本审计记录。版本号必须由服务层在同一事务中计算。
pub(crate) fn insert_version(db: &Database, v: &IdentityVersion) -> Result<()> {
    insert_version_on(db.conn(), v)
}

fn insert_version_on(conn: &Connection, v: &IdentityVersion) -> Result<()> {
    conn.execute(
        "INSERT INTO self_profile_versions \
         (version, snapshot, proposal_id, source, reason, created_at, operator, \
          rolled_back_from) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            v.version,
            v.snapshot,
            v.proposal_id,
            v.source,
            v.reason,
            v.created_at.to_rfc3339(),
            v.operator,
            v.rolled_back_from,
        ],
    )?;
    Ok(())
}

pub(crate) fn get_version(db: &Database, version: i64) -> Result<Option<IdentityVersion>> {
    get_version_on(db.conn(), version)
}

fn get_version_on(conn: &Connection, version: i64) -> Result<Option<IdentityVersion>> {
    let sql = format!("SELECT {VERSION_COLUMNS} FROM self_profile_versions WHERE version = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map([version], map_version)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// 最新版本记录；没有版本时返回 None。
pub(crate) fn latest_version(db: &Database) -> Result<Option<IdentityVersion>> {
    latest_version_on(db.conn())
}

/// 在调用方已持有的连接上读取最新版本。
///
/// 供 `Profile` 读路径复用现有 DB 锁；不构造第二个 `Database`，也不额外加锁。
pub(crate) fn latest_version_on(conn: &Connection) -> Result<Option<IdentityVersion>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS} FROM self_profile_versions ORDER BY version DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
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
        decided_by: row.get(13)?,
        applied_at: row
            .get::<_, Option<String>>(14)?
            .map(parse_utc)
            .transpose()?,
        rolled_back_at: row
            .get::<_, Option<String>>(15)?
            .map(parse_utc)
            .transpose()?,
    })
}

#[cfg(test)]
mod evolution_schedule_tests {
    use super::*;
    use crate::db::Database;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(secs)
    }

    #[test]
    fn evolution_check_anchor_persists_and_parses_utc() {
        let db = Database::in_memory().unwrap();
        let first = at(0);
        let second = at(86_400);

        assert_eq!(latest_evolution_check(&db).unwrap(), None);
        mark_evolution_checked(&db, first).unwrap();
        assert_eq!(latest_evolution_check(&db).unwrap(), Some(first));
        mark_evolution_checked(&db, second).unwrap();
        assert_eq!(latest_evolution_check(&db).unwrap(), Some(second));
    }
}

fn map_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdentityVersion> {
    Ok(IdentityVersion {
        version: row.get(0)?,
        snapshot: row.get(1)?,
        proposal_id: row.get(2)?,
        source: row.get(3)?,
        reason: row.get(4)?,
        created_at: parse_utc(row.get::<_, String>(5)?)?,
        operator: row.get(6)?,
        rolled_back_from: row.get(7)?,
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

fn proposal_on(conn: &Connection, id: &str) -> Result<Option<IdentityChangeProposal>> {
    let sql = format!("SELECT {PROPOSAL_COLUMNS} FROM identity_change_proposals WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map([id], map_proposal)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

fn decide_proposal_on(
    conn: &Connection,
    id: &str,
    status: &str,
    operator: &str,
    decided_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE identity_change_proposals SET status = ?1, decided_at = ?2, decided_by = ?3 \
         WHERE id = ?4 AND status = 'pending'",
        params![status, decided_at.to_rfc3339(), operator, id],
    )?;
    Ok(changed == 1)
}

fn mark_proposal_applied_on(
    conn: &Connection,
    id: &str,
    applied_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE identity_change_proposals SET status = 'applied', applied_at = ?1 \
         WHERE id = ?2 AND status = 'approved'",
        params![applied_at.to_rfc3339(), id],
    )?;
    Ok(changed == 1)
}

fn mark_proposal_rolled_back_on(
    conn: &Connection,
    id: &str,
    rolled_back_at: DateTime<Utc>,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE identity_change_proposals SET status = 'rolled_back', rolled_back_at = ?1 \
         WHERE id = ?2 AND status = 'applied'",
        params![rolled_back_at.to_rfc3339(), id],
    )?;
    Ok(changed == 1)
}

fn current_identity_on(conn: &Connection, now: DateTime<Utc>) -> Result<AgentIdentity> {
    match latest_version_on(conn)? {
        Some(version) => AgentIdentity::from_snapshot_json(
            version.version_number()?,
            version.created_at,
            &version.snapshot,
        )
        .map_err(identity_error),
        None => {
            let rows = crate::db::self_profile::all(conn)?;
            AgentIdentity::from_legacy_rows(AgentIdentity::UNVERSIONED, now, rows)
                .map_err(identity_error)
        }
    }
}

fn ensure_base_version(proposal: &IdentityChangeProposal, current: u64) -> Result<()> {
    let expected = u64::try_from(proposal.base_version).map_err(|_| {
        SqlError::Config(format!(
            "提案 {} 的 base_version {} 不是非负整数",
            proposal.id, proposal.base_version
        ))
    })?;
    if current != expected {
        return Err(SqlError::Config(format!(
            "提案 {} 基线版本为 {}，当前身份版本为 {}；拒绝覆盖并发变更",
            proposal.id, expected, current
        )));
    }
    Ok(())
}

fn ensure_before_value(
    proposal: &IdentityChangeProposal,
    field: IdentityFieldKey,
    base: &AgentIdentity,
) -> Result<()> {
    let current = base.get(field).map(IdentityFieldValue::as_str);
    if current != proposal.before_value.as_deref() {
        return Err(SqlError::Config(format!(
            "提案 {} 的 before_value 已与字段 {} 当前值不一致，拒绝应用",
            proposal.id, field
        )));
    }
    Ok(())
}

fn next_version_on(conn: &Connection) -> Result<u64> {
    match latest_version_on(conn)? {
        Some(version) => version
            .version_number()?
            .checked_add(1)
            .ok_or_else(|| SqlError::Config("身份版本已到数据库上限".to_string())),
        None => Ok(AgentIdentity::INITIAL_VERSION),
    }
}

fn latest_version_before_on(conn: &Connection, version: i64) -> Result<Option<IdentityVersion>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS} FROM self_profile_versions \
         WHERE version < ?1 ORDER BY version DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map([version], map_version)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

fn snapshot_before_applying(
    current_snapshot: &str,
    field: IdentityFieldKey,
    before_value: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String> {
    let current =
        AgentIdentity::from_snapshot_json(0, now, current_snapshot).map_err(identity_error)?;
    let mut fields = current.fields().clone();
    match before_value {
        Some(value) => {
            let value = IdentityFieldValue::new(field, value).map_err(identity_error)?;
            fields.insert(field, value);
        }
        None => {
            fields.remove(&field);
        }
    }
    AgentIdentity::new(0, now, fields)
        .to_snapshot_json()
        .map_err(Into::into)
}

fn replace_profile_field_on(
    conn: &Connection,
    field: IdentityFieldKey,
    value: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    let keys = std::iter::once(field.as_str()).chain(field.aliases().iter().copied());
    for key in keys {
        conn.execute("DELETE FROM self_profile WHERE key = ?1", [key])?;
    }
    if let Some(value) = value {
        conn.execute(
            "INSERT INTO self_profile (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![field.as_str(), value, now.to_rfc3339()],
        )?;
    }
    Ok(())
}

fn replace_profile_snapshot_on(
    conn: &Connection,
    snapshot: &AgentIdentity,
    now: DateTime<Utc>,
) -> Result<()> {
    for field in IdentityFieldKey::ALL {
        let keys = std::iter::once(field.as_str()).chain(field.aliases().iter().copied());
        for key in keys {
            conn.execute("DELETE FROM self_profile WHERE key = ?1", [key])?;
        }
        if let Some(value) = snapshot.get(field) {
            conn.execute(
                "INSERT INTO self_profile (key, value, updated_at) VALUES (?1, ?2, ?3)",
                params![field.as_str(), value.as_str(), now.to_rfc3339()],
            )?;
        }
    }
    Ok(())
}

fn mark_applied_versions_after_on(
    conn: &Connection,
    target_version: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT proposal_id FROM self_profile_versions
         WHERE version > ?1 AND proposal_id IS NOT NULL AND source = 'evolution_agent'",
    )?;
    let proposal_ids = stmt
        .query_map([target_version], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for proposal_id in proposal_ids {
        mark_proposal_rolled_back_on(conn, &proposal_id, now)?;
    }
    Ok(())
}

fn identity_error(error: IdentityError) -> SqlError {
    SqlError::Config(format!("身份数据无效：{error}"))
}

impl IdentityVersion {
    fn version_number(&self) -> Result<u64> {
        u64::try_from(self.version)
            .ok()
            .filter(|version| *version >= AgentIdentity::INITIAL_VERSION)
            .ok_or_else(|| SqlError::Config(format!("身份版本 {} 不是正整数", self.version)))
    }
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
            decided_by: None,
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
            operator: Some("test".to_string()),
            rolled_back_from: None,
        }
    }

    fn legacy_profile(db: &Database, rows: &[(&str, &str)]) {
        for (key, value) in rows {
            crate::db::self_profile::set(db.conn(), key, value).unwrap();
        }
    }

    fn proposal_for(
        id: &str,
        field_key: &str,
        before_value: Option<&str>,
        after_value: &str,
        base_version: i64,
        status: &str,
    ) -> IdentityChangeProposal {
        IdentityChangeProposal {
            id: id.to_string(),
            field_key: field_key.to_string(),
            before_value: before_value.map(str::to_string),
            after_value: after_value.to_string(),
            reason: "用户明确表达偏好".to_string(),
            evidence: r#"["m-1"]"#.to_string(),
            confidence: "medium".to_string(),
            risk_level: "low".to_string(),
            status: status.to_string(),
            proposer: "identity-evolution-v1".to_string(),
            base_version,
            created_at: at(0),
            decided_at: None,
            decided_by: None,
            applied_at: None,
            rolled_back_at: None,
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
    fn field_cooldown_query_includes_all_statuses_and_respects_window() {
        let db = Database::in_memory().unwrap();
        let mut rejected = proposal("rejected", "rejected");
        rejected.created_at = at(1);
        insert_proposal(&db, &rejected).unwrap();

        assert!(
            has_proposal_for_field_between(&db, "voice_style", at(0), at(2)).unwrap(),
            "rejected 提案也必须计入冷却"
        );
        assert!(!has_proposal_for_field_between(&db, "personality", at(0), at(2)).unwrap());
        assert!(!has_proposal_for_field_between(&db, "voice_style", at(2), at(3)).unwrap());
        assert!(!has_proposal_for_field_between(&db, "voice_style", at(2), at(0)).unwrap());
    }

    #[test]
    fn proposal_state_updates_only_from_expected_previous_state() {
        let db = Database::in_memory().unwrap();
        insert_proposal(&db, &proposal("p1", "pending")).unwrap();

        assert!(decide_proposal(&db, "p1", "approved", "test", at(1)).unwrap());
        assert!(
            !decide_proposal(&db, "p1", "rejected", "test", at(2)).unwrap(),
            "approved 不能再被 rejected"
        );
        assert_eq!(get_proposal(&db, "p1").unwrap().unwrap().status, "approved");
    }

    #[test]
    fn proposal_can_only_apply_after_approval_and_rollback_after_apply() {
        let db = Database::in_memory().unwrap();
        insert_proposal(&db, &proposal("p1", "pending")).unwrap();

        assert!(!mark_proposal_applied(&db, "p1", at(1)).unwrap());
        assert!(decide_proposal(&db, "p1", "approved", "test", at(2)).unwrap());
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

    #[test]
    fn applying_pending_proposal_updates_profile_and_audit_atomically() {
        let db = Database::in_memory().unwrap();
        legacy_profile(&db, &[("性格", "慢热"), ("爱好", "旧书店")]);
        insert_proposal(
            &db,
            &proposal_for(
                "p1",
                "voice_style",
                None,
                "更愿意主动分享自己的看法",
                0,
                "pending",
            ),
        )
        .unwrap();

        let applied = approve_and_apply_proposal(&db, "p1", "test", at(10)).unwrap();
        assert_eq!(applied.version, 1);
        assert_eq!(applied.proposal_id.as_deref(), Some("p1"));
        assert_eq!(applied.source, "evolution_agent");
        assert_eq!(
            applied.snapshot,
            r#"{"interests":"旧书店","personality":"慢热","voice_style":"更愿意主动分享自己的看法"}"#
        );

        let p = get_proposal(&db, "p1").unwrap().unwrap();
        assert_eq!(p.status, "applied");
        assert_eq!(p.decided_at, Some(at(10)));
        assert_eq!(p.decided_by.as_deref(), Some("test"));
        assert_eq!(p.applied_at, Some(at(10)));

        let profile = crate::db::self_profile::all(db.conn()).unwrap();
        assert_eq!(
            profile,
            vec![
                ("性格".to_string(), "慢热".to_string()),
                ("爱好".to_string(), "旧书店".to_string()),
                (
                    "voice_style".to_string(),
                    "更愿意主动分享自己的看法".to_string()
                ),
            ],
            "应用只替换目标字段，其他旧 KV 行保留"
        );
        assert_eq!(latest_version(&db).unwrap(), Some(applied));
    }

    #[test]
    fn applying_approved_proposal_keeps_original_decision_time() {
        let db = Database::in_memory().unwrap();
        insert_proposal(
            &db,
            &proposal_for("p1", "voice_style", None, "短句", 0, "pending"),
        )
        .unwrap();
        assert!(decide_proposal(&db, "p1", "approved", "test", at(2)).unwrap());

        let applied = approve_and_apply_proposal(&db, "p1", "test", at(3)).unwrap();
        let p = get_proposal(&db, "p1").unwrap().unwrap();
        assert_eq!(p.status, "applied");
        assert_eq!(p.decided_at, Some(at(2)));
        assert_eq!(p.applied_at, Some(at(3)));
        assert_eq!(applied.version, 1);
    }

    #[test]
    fn base_version_or_before_value_drift_rejects_application_without_writes() {
        let db = Database::in_memory().unwrap();
        insert_version(&db, &version(1)).unwrap();
        insert_proposal(
            &db,
            &proposal_for("stale", "voice_style", Some("v1"), "new", 0, "pending"),
        )
        .unwrap();
        let err = approve_and_apply_proposal(&db, "stale", "test", at(10)).unwrap_err();
        assert!(err.to_string().contains("基线版本"));

        insert_proposal(
            &db,
            &proposal_for("drift", "voice_style", Some("other"), "new", 1, "pending"),
        )
        .unwrap();
        let err = approve_and_apply_proposal(&db, "drift", "test", at(10)).unwrap_err();
        assert!(err.to_string().contains("before_value"));

        let versions = list_versions(&db, 10).unwrap();
        assert_eq!(versions.len(), 1, "失败不得追加版本");
        assert_eq!(
            get_proposal(&db, "stale").unwrap().unwrap().status,
            "pending",
            "失败不得改变提案状态"
        );
        assert_eq!(
            get_proposal(&db, "drift").unwrap().unwrap().status,
            "pending",
            "失败不得改变提案状态"
        );
        assert!(
            crate::db::self_profile::all(db.conn()).unwrap().is_empty(),
            "失败不得写当前身份"
        );
    }

    #[test]
    fn non_applicable_statuses_are_rejected() {
        let db = Database::in_memory().unwrap();
        for (id, status) in [
            ("rejected", "rejected"),
            ("applied", "applied"),
            ("rolled_back", "rolled_back"),
        ] {
            insert_proposal(
                &db,
                &proposal_for(id, "voice_style", None, "新值", 0, status),
            )
            .unwrap();
            assert!(
                approve_and_apply_proposal(&db, id, "test", at(10)).is_err(),
                "{status} 不应再次应用"
            );
        }
        assert!(list_versions(&db, 10).unwrap().is_empty());
    }

    #[test]
    fn applying_a_new_field_removes_legacy_aliases() {
        let db = Database::in_memory().unwrap();
        legacy_profile(&db, &[("语气", "旧语气"), ("说话风格", "旧风格")]);
        insert_proposal(
            &db,
            &proposal_for(
                "p1",
                "voice_style",
                Some("旧语气；旧风格"),
                "新语气",
                0,
                "pending",
            ),
        )
        .unwrap();
        approve_and_apply_proposal(&db, "p1", "test", at(10)).unwrap();

        let profile = crate::db::self_profile::all(db.conn()).unwrap();
        assert_eq!(
            profile,
            vec![("voice_style".to_string(), "新语气".to_string())],
            "canonical key 写入后旧别名不应继续影响回退渲染"
        );
    }

    #[test]
    fn rollback_restores_previous_version_and_records_audit() {
        let db = Database::in_memory().unwrap();
        insert_version(
            &db,
            &IdentityVersion {
                version: 1,
                snapshot: r#"{"personality":"慢热","voice_style":"旧语气"}"#.to_string(),
                proposal_id: None,
                source: "manual".to_string(),
                reason: "初始版本".to_string(),
                created_at: at(0),
                operator: None,
                rolled_back_from: None,
            },
        )
        .unwrap();
        legacy_profile(&db, &[("性格", "慢热"), ("语气", "旧语气")]);
        insert_proposal(
            &db,
            &proposal_for("p1", "voice_style", Some("旧语气"), "新语气", 1, "pending"),
        )
        .unwrap();
        approve_and_apply_proposal(&db, "p1", "test", at(10)).unwrap();

        let rollback = rollback_latest_applied(&db, "test", at(20)).unwrap();
        assert_eq!(rollback.version, 3);
        assert_eq!(rollback.source, "rollback");
        assert_eq!(rollback.proposal_id.as_deref(), Some("p1"));
        assert_eq!(rollback.rolled_back_from, Some(2));
        assert_eq!(
            rollback.snapshot,
            r#"{"personality":"慢热","voice_style":"旧语气"}"#
        );

        let p = get_proposal(&db, "p1").unwrap().unwrap();
        assert_eq!(p.status, "rolled_back");
        assert_eq!(p.rolled_back_at, Some(at(20)));
        assert_eq!(
            crate::db::self_profile::all(db.conn()).unwrap(),
            vec![
                ("性格".to_string(), "慢热".to_string()),
                ("voice_style".to_string(), "旧语气".to_string()),
            ]
        );
        assert_eq!(latest_version(&db).unwrap(), Some(rollback));
    }

    #[test]
    fn rollback_first_applied_change_restores_unversioned_before_value() {
        let db = Database::in_memory().unwrap();
        legacy_profile(&db, &[("性格", "慢热"), ("语气", "旧语气")]);
        insert_proposal(
            &db,
            &proposal_for("p1", "voice_style", Some("旧语气"), "新语气", 0, "pending"),
        )
        .unwrap();
        approve_and_apply_proposal(&db, "p1", "test", at(10)).unwrap();

        let rollback = rollback_latest_applied(&db, "test", at(20)).unwrap();
        assert_eq!(rollback.version, 2);
        assert_eq!(rollback.rolled_back_from, Some(1));
        assert_eq!(
            rollback.snapshot,
            r#"{"personality":"慢热","voice_style":"旧语气"}"#
        );
        assert_eq!(
            crate::db::self_profile::all(db.conn()).unwrap(),
            vec![
                ("性格".to_string(), "慢热".to_string()),
                ("voice_style".to_string(), "旧语气".to_string()),
            ]
        );
    }

    #[test]
    fn rollback_requires_latest_version_to_be_the_applied_proposal() {
        let db = Database::in_memory().unwrap();
        insert_version(
            &db,
            &IdentityVersion {
                version: 1,
                snapshot: r#"{"voice_style":"新值"}"#.to_string(),
                proposal_id: Some("p1".to_string()),
                source: "evolution_agent".to_string(),
                reason: "提案版本".to_string(),
                created_at: at(0),
                operator: None,
                rolled_back_from: None,
            },
        )
        .unwrap();
        insert_proposal(
            &db,
            &proposal_for("p1", "voice_style", None, "新值", 0, "approved"),
        )
        .unwrap();
        let err = rollback_latest_applied(&db, "test", at(10)).unwrap_err();
        assert!(
            err.to_string().contains("不是 applied"),
            "最新版本指向的提案不是 applied 时必须拒绝回滚：{err}"
        );
        assert_eq!(get_proposal(&db, "p1").unwrap().unwrap().status, "approved");
        assert_eq!(list_versions(&db, 10).unwrap().len(), 1);
    }

    #[test]
    fn rollback_without_applied_change_is_rejected_without_writes() {
        let db = Database::in_memory().unwrap();
        let err = rollback_latest_applied(&db, "test", at(10)).unwrap_err();
        assert!(matches!(err, SqlError::Config(_)));
        assert!(list_versions(&db, 10).unwrap().is_empty());
    }

    #[test]
    fn rollback_to_historical_version_restores_full_snapshot_and_audits() {
        let db = Database::in_memory().unwrap();
        insert_version(
            &db,
            &IdentityVersion {
                version: 1,
                snapshot: r#"{"personality":"慢热","voice_style":"旧语气"}"#.to_string(),
                proposal_id: None,
                source: "manual".to_string(),
                reason: "初始版本".to_string(),
                created_at: at(0),
                operator: None,
                rolled_back_from: None,
            },
        )
        .unwrap();
        legacy_profile(&db, &[("性格", "慢热"), ("语气", "旧语气")]);
        insert_proposal(
            &db,
            &proposal_for("p1", "voice_style", Some("旧语气"), "新语气", 1, "pending"),
        )
        .unwrap();
        approve_and_apply_proposal(&db, "p1", "test", at(10)).unwrap();
        insert_proposal(
            &db,
            &proposal_for("p2", "interests", None, "旧书店", 2, "pending"),
        )
        .unwrap();
        approve_and_apply_proposal(&db, "p2", "test", at(20)).unwrap();

        let restored = rollback_to_version(&db, 1, "test", at(30)).unwrap();
        assert_eq!(restored.version, 4);
        assert_eq!(restored.source, "rollback");
        assert_eq!(restored.proposal_id, None);
        assert_eq!(restored.rolled_back_from, Some(3));
        assert_eq!(
            restored.snapshot,
            r#"{"personality":"慢热","voice_style":"旧语气"}"#
        );
        assert_eq!(
            crate::db::self_profile::all(db.conn()).unwrap(),
            vec![
                ("personality".to_string(), "慢热".to_string()),
                ("voice_style".to_string(), "旧语气".to_string()),
            ],
            "恢复到目标快照时必须删除目标之后新增的 canonical 字段"
        );
        assert_eq!(
            get_proposal(&db, "p1").unwrap().unwrap().status,
            "rolled_back"
        );
        assert_eq!(
            get_proposal(&db, "p2").unwrap().unwrap().status,
            "rolled_back"
        );
        assert_eq!(latest_version(&db).unwrap(), Some(restored));
    }

    #[test]
    fn rollback_to_current_or_missing_version_is_rejected_without_writes() {
        let db = Database::in_memory().unwrap();
        insert_version(
            &db,
            &IdentityVersion {
                version: 1,
                snapshot: r#"{"voice_style":"当前"}"#.to_string(),
                proposal_id: None,
                source: "manual".to_string(),
                reason: "当前版本".to_string(),
                created_at: at(0),
                operator: None,
                rolled_back_from: None,
            },
        )
        .unwrap();
        insert_version(
            &db,
            &IdentityVersion {
                version: 3,
                snapshot: r#"{"voice_style":"最新"}"#.to_string(),
                proposal_id: None,
                source: "manual".to_string(),
                reason: "最新版本".to_string(),
                created_at: at(1),
                operator: None,
                rolled_back_from: None,
            },
        )
        .unwrap();
        let err = rollback_to_version(&db, 3, "test", at(10)).unwrap_err();
        assert!(err.to_string().contains("必须早于当前版本"));
        assert_eq!(list_versions(&db, 10).unwrap().len(), 2);

        let err = rollback_to_version(&db, 2, "test", at(11)).unwrap_err();
        assert!(matches!(err, SqlError::NotFound(_)), "{err}");
        assert_eq!(list_versions(&db, 10).unwrap().len(), 2);
    }
}
