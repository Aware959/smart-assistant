//! 身份提案服务测试：校验、证据、风险、限流与审批状态机。

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use super::*;
use crate::core::identity::{IdentityFieldKey, ProposalStatus, RiskLevel};
use crate::db::Database;

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::seconds(secs)
}

fn db() -> Arc<Mutex<Database>> {
    Arc::new(Mutex::new(Database::in_memory().unwrap()))
}

fn add_message(db: &Arc<Mutex<Database>>, role: &str, content: &str) -> String {
    let guard = crate::db::lock_db(db);
    let session = crate::db::session::create(&guard, "test").unwrap();
    crate::db::message::create(&guard, &session.id, role, content)
        .unwrap()
        .id
}

fn draft(evidence: Vec<String>) -> ProposalDraft {
    ProposalDraft {
        field_key: "voice_style".to_string(),
        after_value: "更愿意主动分享自己的看法，但仍然保持短句".to_string(),
        reason: "用户多次表示喜欢有观点的回复".to_string(),
        evidence_message_ids: evidence,
        confidence: "medium".to_string(),
        proposer: "identity-evolution-v1".to_string(),
        source: ChangeSource::EvolutionAgent,
    }
}

fn service(db: Arc<Mutex<Database>>) -> IdentityService {
    IdentityService::new(db)
}

fn limited_service(db: Arc<Mutex<Database>>, limits: ProposalLimits) -> IdentityService {
    IdentityService::with_limits(db, limits)
}

#[test]
fn creates_pending_proposal_without_touching_current_identity() {
    let db = db();
    let evidence = add_message(&db, "user", "我喜欢你有自己的看法");
    let service = service(db.clone());
    let created = service
        .create_proposal(draft(vec![evidence.clone()]), at(0))
        .unwrap();

    assert_eq!(created.proposal.status, "pending");
    assert_eq!(created.proposal.field_key, "voice_style");
    assert_eq!(created.proposal.before_value, None);
    assert_eq!(created.proposal.base_version, 0);
    assert_eq!(created.proposal.risk_level, "medium");
    assert_eq!(
        created.proposal.evidence,
        serde_json::json!([evidence]).to_string()
    );

    let guard = crate::db::lock_db(&db);
    assert_eq!(
        crate::db::identity::list_proposals_by_status(&guard, "pending", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        crate::db::identity::latest_version(&guard).unwrap(),
        None,
        "创建提案不得写身份版本"
    );
    assert!(
        crate::db::self_profile::all(guard.conn())
            .unwrap()
            .is_empty(),
        "创建提案不得修改当前身份"
    );
}

#[test]
fn before_value_comes_from_latest_version_not_from_draft() {
    let db = db();
    let evidence = add_message(&db, "user", "你最近说话太长了");
    {
        let guard = crate::db::lock_db(&db);
        crate::db::identity::insert_version(
            &guard,
            &IdentityVersion {
                version: 1,
                snapshot: r#"{"voice_style":"喜欢长篇解释"}"#.to_string(),
                proposal_id: None,
                source: "manual".to_string(),
                reason: "初始版本".to_string(),
                created_at: at(0),
                rolled_back_from: None,
            },
        )
        .unwrap();
    }

    let created = service(db)
        .create_proposal(draft(vec![evidence]), at(1))
        .unwrap();
    assert_eq!(
        created.proposal.before_value.as_deref(),
        Some("喜欢长篇解释")
    );
    assert_eq!(created.proposal.base_version, 1);
}

#[test]
fn rejects_unknown_fields_empty_values_and_control_characters() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = service(db);

    let mut invalid = draft(vec![evidence.clone()]);
    invalid.field_key = "mood".to_string();
    assert!(service.create_proposal(invalid, at(0)).is_err());

    let mut invalid = draft(vec![evidence.clone()]);
    invalid.after_value = "  ".to_string();
    assert!(service.create_proposal(invalid, at(0)).is_err());

    let mut invalid = draft(vec![evidence]);
    invalid.after_value = "坏\u{0007}人".to_string();
    assert!(service.create_proposal(invalid, at(0)).is_err());
}

#[test]
fn rejects_empty_or_missing_evidence() {
    let db = db();
    let service = service(db.clone());
    assert!(service.create_proposal(draft(vec![]), at(0)).is_err());

    let missing = service.create_proposal(draft(vec!["missing".to_string()]), at(0));
    assert!(missing.is_err());
}

#[test]
fn evidence_ids_are_trimmed_deduplicated_and_ordered() {
    let db = db();
    let first = add_message(&db, "user", "第一条");
    let second = add_message(&db, "user", "第二条");
    let created = service(db)
        .create_proposal(
            draft(vec![format!(" {first} "), second.clone(), first.clone()]),
            at(0),
        )
        .unwrap();
    assert_eq!(
        created.proposal.evidence,
        serde_json::json!([first, second]).to_string()
    );
}

#[test]
fn rejects_non_evolution_sources_and_invalid_confidence() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = service(db);

    let mut manual = draft(vec![evidence.clone()]);
    manual.source = ChangeSource::Manual;
    assert!(service.create_proposal(manual, at(0)).is_err());

    let mut invalid = draft(vec![evidence]);
    invalid.confidence = "certain".to_string();
    assert!(service.create_proposal(invalid, at(0)).is_err());
}

#[test]
fn risk_is_derived_from_field_and_change_size() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = service(db);

    let mut high = draft(vec![evidence]);
    high.field_key = "values".to_string();
    high.after_value = "只按利益行事".to_string();
    let high = service.create_proposal(high, at(1)).unwrap();
    assert_eq!(high.proposal.risk_level, "high");

    assert_eq!(
        assess_risk(
            IdentityFieldKey::RelationshipStance,
            Some("保持温和但有边界"),
            "保持温和但有边界"
        )
        .unwrap(),
        RiskLevel::High
    );
}

#[test]
fn enforces_pending_limit_and_per_field_cooldown() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = limited_service(
        db,
        ProposalLimits {
            field_cooldown: Duration::days(7),
            max_pending: 3,
        },
    );

    let mut first = draft(vec![evidence.clone()]);
    first.field_key = "personality".to_string();
    service.create_proposal(first, at(0)).unwrap();

    let mut same_field = draft(vec![evidence.clone()]);
    same_field.field_key = "personality".to_string();
    assert!(
        service.create_proposal(same_field, at(1)).is_err(),
        "同一字段冷却期内应拒绝"
    );

    let mut other = draft(vec![evidence.clone()]);
    other.field_key = "interests".to_string();
    service.create_proposal(other, at(2)).unwrap();
    let mut third = draft(vec![evidence.clone()]);
    third.field_key = "voice_style".to_string();
    service.create_proposal(third, at(3)).unwrap();

    let mut fourth = draft(vec![evidence]);
    fourth.field_key = "self_concept".to_string();
    let err = service
        .create_proposal(fourth, at(4))
        .unwrap_err()
        .to_string();
    assert!(err.contains("上限"), "达到 pending 上限后应拒绝：{err}");
}

#[test]
fn field_cooldown_includes_rejected_proposals() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = limited_service(
        db,
        ProposalLimits {
            field_cooldown: Duration::days(7),
            max_pending: 10,
        },
    );

    let mut first = draft(vec![evidence.clone()]);
    first.field_key = "personality".to_string();
    let first = service.create_proposal(first, at(0)).unwrap();
    service
        .decide(&first.proposal.id, ProposalStatus::Rejected, false, at(1))
        .unwrap();

    let mut same_field = draft(vec![evidence]);
    same_field.field_key = "personality".to_string();
    let err = service
        .create_proposal(same_field, at(2))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("冷却"),
        "已拒绝提案仍应占用同字段冷却期：{err}"
    );
}

#[test]
fn pending_and_get_are_scoped_to_pending_proposals() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = service(db);
    let created = service
        .create_proposal(draft(vec![evidence]), at(0))
        .unwrap();
    assert_eq!(service.pending(10).unwrap().len(), 1);
    assert_eq!(service.get(&created.proposal.id).unwrap().status, "pending");
    assert!(service.get("missing").is_err());
}

#[test]
fn decision_is_pending_only_and_rejection_has_no_consent_requirement() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = service(db);
    let created = service
        .create_proposal(draft(vec![evidence]), at(0))
        .unwrap();

    let rejected = service
        .decide(&created.proposal.id, ProposalStatus::Rejected, false, at(1))
        .unwrap();
    assert_eq!(rejected.status, "rejected");
    assert!(
        service
            .decide(&created.proposal.id, ProposalStatus::Approved, false, at(2))
            .is_err(),
        "已决提案不能再次审批"
    );
    assert!(
        service
            .decide(&created.proposal.id, ProposalStatus::Applied, false, at(3))
            .is_err(),
        "审批入口不能直接应用"
    );
}

#[test]
fn relationship_stance_approval_requires_explicit_consent() {
    let db = db();
    let evidence = add_message(&db, "user", "关系边界");
    let service = service(db);
    let mut draft = draft(vec![evidence]);
    draft.field_key = "relationship_stance".to_string();
    draft.after_value = "更亲近，但尊重独处".to_string();
    let created = service.create_proposal(draft, at(0)).unwrap();

    assert!(
        service
            .decide(&created.proposal.id, ProposalStatus::Approved, false, at(1))
            .is_err(),
        "关系姿态缺少明确同意时不能批准"
    );
    let approved = service
        .decide(&created.proposal.id, ProposalStatus::Approved, true, at(2))
        .unwrap();
    assert_eq!(approved.status, "approved");
}

#[test]
fn proposal_view_exposes_structured_evidence() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let created = service(db)
        .create_proposal(draft(vec![evidence.clone()]), at(0))
        .unwrap();
    let view = ProposalView::try_from(&created.proposal).unwrap();
    assert_eq!(view.evidence_message_ids, vec![evidence]);
    assert_eq!(view.status, "pending");
    assert_eq!(view.risk_level, "medium");
}

#[test]
fn proposal_limits_have_mvp_defaults() {
    let limits = ProposalLimits::default();
    assert_eq!(limits.field_cooldown, Duration::days(7));
    assert_eq!(limits.max_pending, 3);
}
