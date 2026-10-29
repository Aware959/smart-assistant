//! 身份提案服务：校验、限流、持久化与审批状态转换。
//!
//! 本服务**不修改当前身份**。它只把模型给出的“想改什么”变成可审计的 pending 提案，
//! 并由人工审批决定 approved / rejected。事务应用、版本写入与回滚属于 T7，调用方
//! 仍不能经本服务直接改变提示词。
//!
//! `before_value`、`base_version`、风险等级和初始状态都由代码推导，不采信模型自报；
//! 模型只提供目标字段、after 值、理由、证据、置信度和来源。

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::core::identity::{
    AgentIdentity, ChangeSource, IdentityError, IdentityFieldKey, IdentityFieldValue,
    ProposalStatus, RiskLevel,
};
use crate::db::identity::{IdentityChangeProposal, IdentityVersion};
use crate::db::Database;
use crate::error::{Result, SqlError};

/// 模型提交的结构化提案草稿。
///
/// `risk` 字段刻意不存在：风险由代码按字段与变更幅度评估。`evidence` 只接受已存在的
/// 消息 id；没有证据的提案不落库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalDraft {
    pub field_key: String,
    pub after_value: String,
    pub reason: String,
    pub evidence_message_ids: Vec<String>,
    pub confidence: String,
    pub proposer: String,
    pub source: ChangeSource,
}

/// 提案创建成功后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedProposal {
    pub proposal: IdentityChangeProposal,
}

/// 可注入的限流规则。MVP 默认值来自已确认决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalLimits {
    /// 同一字段两次提案之间的冷却期。
    pub field_cooldown: Duration,
    /// 当前 pending 提案数量上限。
    pub max_pending: usize,
}

impl Default for ProposalLimits {
    fn default() -> Self {
        Self {
            field_cooldown: Duration::days(7),
            max_pending: 3,
        }
    }
}

/// 身份提案服务。
pub struct IdentityService {
    db: Arc<Mutex<Database>>,
    limits: ProposalLimits,
}

impl IdentityService {
    pub(crate) fn new(db: Arc<Mutex<Database>>) -> Self {
        Self::with_limits(db, ProposalLimits::default())
    }

    fn with_limits(db: Arc<Mutex<Database>>, limits: ProposalLimits) -> Self {
        Self { db, limits }
    }

    /// 创建一条 pending 提案。不会写 `self_profile` 或身份版本。
    pub fn create_proposal(
        &self,
        draft: ProposalDraft,
        now: DateTime<Utc>,
    ) -> Result<CreatedProposal> {
        let field = IdentityFieldKey::parse(&draft.field_key).map_err(identity_error)?;
        let after = IdentityFieldValue::new(field, &draft.after_value).map_err(identity_error)?;
        let reason = required_text("reason", &draft.reason)?;
        let confidence = parse_confidence(&draft.confidence)?;
        let proposer = required_text("proposer", &draft.proposer)?;
        if draft.source != ChangeSource::EvolutionAgent {
            return Err(SqlError::Config(
                "身份提案只能由 evolution_agent 创建；manual 变更走独立审计入口".to_string(),
            ));
        }

        let guard = crate::db::lock_db(&self.db);
        let base = current_identity(&guard, now)?;
        let before = base.get(field).map(|value| value.as_str().to_string());
        let evidence = validate_evidence(&guard, &draft.evidence_message_ids)?;
        enforce_limits(&guard, field, now, self.limits)?;

        let risk = assess_risk(field, before.as_deref(), after.as_str())?;
        let proposal = IdentityChangeProposal {
            id: uuid::Uuid::new_v4().to_string(),
            field_key: field.as_str().to_string(),
            before_value: before,
            after_value: after.as_str().to_string(),
            reason,
            evidence,
            confidence,
            risk_level: risk.as_str().to_string(),
            status: ProposalStatus::Pending.as_str().to_string(),
            proposer,
            base_version: i64::try_from(base.version()).map_err(|_| {
                SqlError::Config(format!("身份版本 {} 超出数据库范围", base.version()))
            })?,
            created_at: now,
            decided_at: None,
            applied_at: None,
            rolled_back_at: None,
        };
        crate::db::identity::insert_proposal(&guard, &proposal)?;
        Ok(CreatedProposal { proposal })
    }

    /// 列出待审批提案，按创建时刻倒序。
    pub fn pending(&self, limit: usize) -> Result<Vec<IdentityChangeProposal>> {
        let guard = crate::db::lock_db(&self.db);
        crate::db::identity::list_proposals_by_status(
            &guard,
            ProposalStatus::Pending.as_str(),
            limit,
        )
    }

    pub fn get(&self, id: &str) -> Result<IdentityChangeProposal> {
        let guard = crate::db::lock_db(&self.db);
        crate::db::identity::proposal_or_err(&guard, id)
    }

    /// 人工审批：只允许 pending → approved / rejected。
    ///
    /// 关系姿态的批准还需要调用方显式传入用户同意；仅点击“通过”不够。
    pub fn decide(
        &self,
        id: &str,
        status: ProposalStatus,
        explicit_consent: bool,
        now: DateTime<Utc>,
    ) -> Result<IdentityChangeProposal> {
        if !matches!(status, ProposalStatus::Approved | ProposalStatus::Rejected) {
            return Err(SqlError::Config(format!(
                "审批只能产生 approved / rejected，收到 {}",
                status.as_str()
            )));
        }

        let guard = crate::db::lock_db(&self.db);
        let proposal = crate::db::identity::proposal_or_err(&guard, id)?;
        let field = IdentityFieldKey::parse(&proposal.field_key).map_err(identity_error)?;
        if status == ProposalStatus::Approved
            && field.spec().requires_explicit_consent
            && !explicit_consent
        {
            return Err(SqlError::Config(format!(
                "字段 {} 需要用户明确同意，不能仅点击通过",
                field
            )));
        }

        let changed = crate::db::identity::decide_proposal(&guard, id, status.as_str(), now)?;
        if !changed {
            return Err(SqlError::Config(format!(
                "提案 {id} 不是 pending，不能再次审批"
            )));
        }
        crate::db::identity::proposal_or_err(&guard, id)
    }
}

/// 读取当前身份：优先最新版本，否则由旧 KV 构造未版本化身份。
fn current_identity(guard: &Database, now: DateTime<Utc>) -> Result<AgentIdentity> {
    match crate::db::identity::latest_version(guard)? {
        Some(version) => {
            let number = u64::try_from(version.version).map_err(|_| {
                SqlError::Config(format!("身份版本 {} 不是正整数", version.version))
            })?;
            AgentIdentity::from_snapshot_json(number, version.created_at, &version.snapshot)
                .map_err(identity_error)
        }
        None => {
            let rows = crate::db::self_profile::all(guard.conn())?;
            AgentIdentity::from_legacy_rows(AgentIdentity::UNVERSIONED, now, rows)
                .map_err(identity_error)
        }
    }
}

/// 校验证据 id 至少一条，且全部真实存在。
///
/// 返回 canonical JSON 数组，保留调用方给出的顺序并去重。
fn validate_evidence(guard: &Database, ids: &[String]) -> Result<String> {
    let mut seen = BTreeSet::new();
    let mut canonical = Vec::new();
    for raw in ids {
        let id = raw.trim();
        if id.is_empty() {
            return Err(SqlError::Config("证据 id 不能为空".to_string()));
        }
        if !seen.insert(id.to_string()) {
            continue;
        }
        if crate::db::message::get(guard, id)?.is_none() {
            return Err(SqlError::Config(format!("证据消息不存在：{id}")));
        }
        canonical.push(id.to_string());
    }
    if canonical.is_empty() {
        return Err(SqlError::Config("至少需要一条真实的消息证据".to_string()));
    }
    Ok(serde_json::to_string(&canonical)?)
}

/// 频率限制：同一字段 7 天冷却 + 全局 pending 上限。
///
/// 冷却覆盖所有状态的提案，而不只是 pending。否则模型可以先反复提交再拒绝，
/// 绕过“同一字段 7 天内只提一次”的约束。
fn enforce_limits(
    guard: &Database,
    field: IdentityFieldKey,
    now: DateTime<Utc>,
    limits: ProposalLimits,
) -> Result<()> {
    let pending = crate::db::identity::list_proposals_by_status(
        guard,
        ProposalStatus::Pending.as_str(),
        limits.max_pending.saturating_add(1),
    )?;
    if pending.len() >= limits.max_pending {
        return Err(SqlError::Config(format!(
            "待审提案已达上限 {}",
            limits.max_pending
        )));
    }
    let since = now - limits.field_cooldown;
    if crate::db::identity::has_proposal_for_field_between(guard, field.as_str(), since, now)? {
        return Err(SqlError::Config(format!("字段 {field} 在冷却期内已有提案")));
    }
    Ok(())
}

/// 置信度只接受 low / medium / high；不把模型自由文本写进审计表。
fn parse_confidence(raw: &str) -> Result<String> {
    let value = raw.trim().to_ascii_lowercase();
    if matches!(value.as_str(), "low" | "medium" | "high") {
        Ok(value)
    } else {
        Err(SqlError::Config(format!(
            "confidence 必须是 low / medium / high，收到 {raw}"
        )))
    }
}

/// 风险由代码评估：高风险字段固定 high；新增字段至少 medium；大幅改写升为 high。
fn assess_risk(field: IdentityFieldKey, before: Option<&str>, after: &str) -> Result<RiskLevel> {
    let mut risk = if matches!(
        field,
        IdentityFieldKey::SelfConcept
            | IdentityFieldKey::Values
            | IdentityFieldKey::RelationshipStance
    ) {
        RiskLevel::High
    } else {
        RiskLevel::Low
    };

    if before.is_none() {
        risk = risk.max(RiskLevel::Medium);
    }
    if let Some(before) = before {
        let before_len = before.chars().count().max(1);
        let after_len = after.chars().count();
        let delta = before_len.abs_diff(after_len);
        if delta * 100 >= before_len * 50 {
            risk = RiskLevel::High;
        }
    }
    Ok(risk)
}

fn required_text(label: &str, raw: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(SqlError::Config(format!("{label} 不能为空")));
    }
    if value.chars().any(char::is_control) {
        return Err(SqlError::Config(format!("{label} 含控制字符")));
    }
    Ok(value.to_string())
}

fn identity_error(error: IdentityError) -> SqlError {
    SqlError::Config(format!("当前身份无效：{error}"))
}

/// 便于后续审批界面展示的提案视图。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposalView {
    pub id: String,
    pub field_key: String,
    pub before_value: Option<String>,
    pub after_value: String,
    pub reason: String,
    pub evidence_message_ids: Vec<String>,
    pub confidence: String,
    pub risk_level: String,
    pub status: String,
    pub base_version: i64,
}

impl TryFrom<&IdentityChangeProposal> for ProposalView {
    type Error = SqlError;

    fn try_from(value: &IdentityChangeProposal) -> Result<Self> {
        let evidence: Vec<String> = serde_json::from_str(&value.evidence).map_err(|e| {
            SqlError::Config(format!("提案 {} 的证据不是字符串数组：{e}", value.id))
        })?;
        Ok(Self {
            id: value.id.clone(),
            field_key: value.field_key.clone(),
            before_value: value.before_value.clone(),
            after_value: value.after_value.clone(),
            reason: value.reason.clone(),
            evidence_message_ids: evidence,
            confidence: value.confidence.clone(),
            risk_level: value.risk_level.clone(),
            status: value.status.clone(),
            base_version: value.base_version,
        })
    }
}

/// 只读审计视图：当前身份版本（未版本化返回 None）。
pub fn latest_version(db: &Database) -> Result<Option<IdentityVersion>> {
    crate::db::identity::latest_version(db)
}

#[cfg(test)]
#[path = "identity_service_tests.rs"]
mod tests;
