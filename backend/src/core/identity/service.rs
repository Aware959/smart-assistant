//! 身份提案服务：校验、限流、审批、事务应用与回滚。
//!
//! 模型只能创建 pending 提案；人工审批后，[`IdentityService::approve_and_apply`] 在
//! 单个 SQLite 事务中写入当前身份和版本审计，[`IdentityService::rollback`] 恢复当前
//! 生效变更之前的上一条快照。成功提交后显式失效 `Profile` 缓存，失败则整体回滚。
//!
//! `before_value`、`base_version`、风险等级和初始状态都由代码推导，不采信模型自报；
//! 模型只提供目标字段、after 值、理由、证据、置信度和来源。

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::core::identity::profile::Profile;
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
    pub proposal: ProposalView,
}

/// 当前身份的只读视图。
///
/// `version = None` 表示 Profile 读取失败或快照损坏后回退到了旧 KV；`Some(0)` 表示
/// 尚未写入版本审计表的旧 `self_profile`。管理入口只读展示，不直接触碰数据库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityView {
    pub version: Option<u64>,
    pub text: String,
}

/// 一次生效 / 回滚产生的身份版本只读视图。
///
/// 传输层只消费这个视图，不接触版本表行结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityVersionView {
    pub version: i64,
    pub proposal_id: Option<String>,
    pub source: String,
    pub reason: String,
    pub operator: Option<String>,
    pub rolled_back_from: Option<i64>,
}

impl From<&IdentityVersion> for IdentityVersionView {
    fn from(value: &IdentityVersion) -> Self {
        Self {
            version: value.version,
            proposal_id: value.proposal_id.clone(),
            source: value.source.clone(),
            reason: value.reason.clone(),
            operator: value.operator.clone(),
            rolled_back_from: value.rolled_back_from,
        }
    }
}

/// 可注入的限流规则。MVP 默认值来自已确认决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalLimits {
    /// 任意身份提案之间的全局冷却期。后台任务每 24 小时最多运行一次。
    pub global_cooldown: Duration,
    /// 同一字段两次提案之间的冷却期。
    pub field_cooldown: Duration,
    /// 当前 pending 提案数量上限。
    pub max_pending: usize,
}

impl Default for ProposalLimits {
    fn default() -> Self {
        Self {
            global_cooldown: Duration::days(1),
            field_cooldown: Duration::days(7),
            max_pending: 3,
        }
    }
}

/// 身份提案服务。
pub struct IdentityService {
    db: Arc<Mutex<Database>>,
    profile: Arc<Profile>,
    limits: ProposalLimits,
}

/// 后台演化调度是否运行一次的纯策略结果。
///
/// 供 `zhiwei-service` 的任务循环记录“为什么跳过”，不把调度细节塞进身份服务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvolutionGate {
    Run,
    PendingLimit,
    GlobalCooldown,
    Disabled,
}

impl IdentityService {
    pub(crate) fn new(db: Arc<Mutex<Database>>, profile: Arc<Profile>) -> Self {
        Self::with_limits(db, profile, ProposalLimits::default())
    }

    fn with_limits(
        db: Arc<Mutex<Database>>,
        profile: Arc<Profile>,
        limits: ProposalLimits,
    ) -> Self {
        Self {
            db,
            profile,
            limits,
        }
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
            decided_by: None,
            applied_at: None,
            rolled_back_at: None,
        };
        crate::db::identity::insert_proposal(&guard, &proposal)?;
        Ok(CreatedProposal {
            proposal: proposal_view(&proposal)?,
        })
    }

    /// 列出待审批提案，按创建时刻倒序。
    pub fn pending(&self, limit: usize) -> Result<Vec<ProposalView>> {
        let guard = crate::db::lock_db(&self.db);
        let proposals = crate::db::identity::list_proposals_by_status(
            &guard,
            ProposalStatus::Pending.as_str(),
            limit,
        )?;
        proposals.iter().map(proposal_view).collect()
    }

    pub fn get(&self, id: &str) -> Result<ProposalView> {
        let guard = crate::db::lock_db(&self.db);
        let proposal = crate::db::identity::proposal_or_err(&guard, id)?;
        proposal_view(&proposal)
    }

    /// 读取当前身份渲染结果与版本。
    ///
    /// 复用 `Profile` 的版本化渲染和 TTL 缓存；命令层只消费这个视图，不直读
    /// `self_profile` 或版本表。
    pub fn current(&self) -> IdentityView {
        let snapshot = self.profile.snapshot();
        IdentityView {
            version: snapshot.version,
            text: snapshot.text,
        }
    }

    /// 人工审批：只允许 pending → approved / rejected。
    ///
    /// 关系姿态的批准还需要调用方显式传入用户同意；仅点击“通过”不够。
    pub fn decide(
        &self,
        id: &str,
        status: ProposalStatus,
        explicit_consent: bool,
        operator: &str,
        now: DateTime<Utc>,
    ) -> Result<ProposalView> {
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

        let operator = required_text("operator", operator)?;
        let changed =
            crate::db::identity::decide_proposal(&guard, id, status.as_str(), &operator, now)?;
        if !changed {
            return Err(SqlError::Config(format!(
                "提案 {id} 不是 pending，不能再次审批"
            )));
        }
        let proposal = crate::db::identity::proposal_or_err(&guard, id)?;
        proposal_view(&proposal)
    }

    /// 人工批准并在同一事务中应用提案。
    ///
    /// `pending` 会先转为 `approved` 再应用；已由 [`Self::decide`] 批准为 `approved`
    /// 的提案也可直接应用。关系姿态仍要求 `explicit_consent = true`。应用会校验
    /// `base_version` 与 `before_value`，成功后立即失效当前 Profile 缓存。
    pub fn approve_and_apply(
        &self,
        id: &str,
        explicit_consent: bool,
        operator: &str,
        now: DateTime<Utc>,
    ) -> Result<IdentityVersionView> {
        let operator = required_text("operator", operator)?;
        let version = {
            let guard = crate::db::lock_db(&self.db);
            let proposal = crate::db::identity::proposal_or_err(&guard, id)?;
            let field = IdentityFieldKey::parse(&proposal.field_key).map_err(identity_error)?;
            if field.spec().requires_explicit_consent && !explicit_consent {
                return Err(SqlError::Config(format!(
                    "字段 {} 需要用户明确同意，不能直接应用",
                    field
                )));
            }
            crate::db::identity::approve_and_apply_proposal(&guard, id, &operator, now)?
        };
        self.profile.invalidate();
        Ok(IdentityVersionView::from(&version))
    }

    /// 人工拒绝 pending 提案。拒绝不改变当前身份，因此不失效 Profile 缓存。
    pub fn reject(&self, id: &str, operator: &str, now: DateTime<Utc>) -> Result<ProposalView> {
        self.decide(id, ProposalStatus::Rejected, false, operator, now)
    }

    /// 回滚当前最新 applied 提案，恢复上一条完整身份快照。
    ///
    /// 回滚版本与提案状态在同一事务内写入；提交后立即失效 Profile 缓存。
    pub fn rollback(&self, operator: &str, now: DateTime<Utc>) -> Result<IdentityVersionView> {
        let operator = required_text("operator", operator)?;
        let version = {
            let guard = crate::db::lock_db(&self.db);
            crate::db::identity::rollback_latest_applied(&guard, &operator, now)?
        };
        self.profile.invalidate();
        Ok(IdentityVersionView::from(&version))
    }

    /// 回滚到指定历史版本。
    ///
    /// `None` 只允许回滚最新 applied 演化提案。`Some(target)` 允许恢复任意更早的
    /// 已审计快照；恢复动作本身追加一个新的 `rollback` 版本，不删除历史。
    pub fn rollback_version(
        &self,
        version: Option<i64>,
        operator: &str,
        now: DateTime<Utc>,
    ) -> Result<IdentityVersionView> {
        let operator = required_text("operator", operator)?;
        match version {
            None => self.rollback(&operator, now),
            Some(version) => {
                let restored = {
                    let guard = crate::db::lock_db(&self.db);
                    crate::db::identity::rollback_to_version(&guard, version, &operator, now)?
                };
                self.profile.invalidate();
                Ok(IdentityVersionView::from(&restored))
            }
        }
    }

    /// 后台演化是否还应调用模型。
    ///
    /// 在调用 LLM 前先挡住全局 24 小时冷却和 pending 上限，避免白跑一次结构化推理；
    /// 最终落库时 [`Self::create_proposal`] 仍会再次校验，防止并发进程绕过。
    pub fn evolution_run_allowed(&self, now: DateTime<Utc>) -> Result<bool> {
        Ok(self.evolution_gate(now)? == EvolutionGate::Run)
    }

    /// 返回后台演化当前被哪条规则挡住，便于调度日志。
    pub fn evolution_gate(&self, now: DateTime<Utc>) -> Result<EvolutionGate> {
        let guard = crate::db::lock_db(&self.db);
        if self.limits.global_cooldown > Duration::zero()
            && crate::db::identity::has_any_proposal_between(
                &guard,
                now - self.limits.global_cooldown,
                now,
            )?
        {
            return Ok(EvolutionGate::GlobalCooldown);
        }
        let pending = crate::db::identity::list_proposals_by_status(
            &guard,
            ProposalStatus::Pending.as_str(),
            self.limits.max_pending,
        )?;
        if pending.len() >= self.limits.max_pending {
            return Ok(EvolutionGate::PendingLimit);
        }
        Ok(EvolutionGate::Run)
    }

    /// 组装后台演化所需的只读证据快照。
    ///
    /// 这里只读会话最近消息、长期记忆和当前身份；不创建会话、不写记忆，也不调用
    /// LLM。`sessions` 是会话域服务，记忆经 db 适配器读取后只暴露为领域值。
    pub fn evidence_snapshot(
        &self,
        sessions: &crate::core::session::Sessions,
        message_limit: usize,
        memory_limit: usize,
    ) -> Result<crate::core::identity::evolution::EvidenceSnapshot> {
        use crate::core::identity::evolution::{EvidenceMemory, EvidenceMessage, EvidenceSnapshot};

        let identity = self.current();
        let messages = sessions
            .recent_messages_for_mapped_sessions(10, message_limit)?
            .into_iter()
            .filter(|m| !m.content.trim().is_empty())
            .map(|m| EvidenceMessage {
                id: m.id,
                role: m.role,
                content: m.content,
            })
            .collect();
        let memories = {
            let guard = crate::db::lock_db(&self.db);
            crate::db::memory::list(&guard, memory_limit)?
                .into_iter()
                .map(|m| EvidenceMemory {
                    content: m.content,
                    source_message_id: m.message_id,
                })
                .collect()
        };
        Ok(EvidenceSnapshot {
            identity: identity.text,
            messages,
            memories,
        })
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

/// 频率限制：全局每日冷却 + 同一字段 7 天冷却 + pending 上限。
///
/// 冷却覆盖所有状态的提案，而不只是 pending。否则模型可以先反复提交再拒绝，
/// 绕过“同一字段 7 天内只提一次”的约束。
fn enforce_limits(
    guard: &Database,
    field: IdentityFieldKey,
    now: DateTime<Utc>,
    limits: ProposalLimits,
) -> Result<()> {
    if limits.global_cooldown > Duration::zero()
        && crate::db::identity::has_any_proposal_between(guard, now - limits.global_cooldown, now)?
    {
        return Err(SqlError::Config(format!(
            "{} 小时内已有身份提案，后台反思每天最多运行一次",
            limits.global_cooldown.num_hours()
        )));
    }
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
    pub proposer: String,
    /// 审批人；pending 时为空。MVP 入口为 Telegram，值形如 `telegram:<chat_id>`。
    pub decided_by: Option<String>,
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
            proposer: value.proposer.clone(),
            decided_by: value.decided_by.clone(),
            base_version: value.base_version,
        })
    }
}

fn proposal_view(value: &IdentityChangeProposal) -> Result<ProposalView> {
    ProposalView::try_from(value)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
