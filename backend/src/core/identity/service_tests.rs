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
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    IdentityService::new(db, profile)
}

fn limited_service(db: Arc<Mutex<Database>>, limits: ProposalLimits) -> IdentityService {
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    IdentityService::with_limits(db, profile, limits)
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
    assert_eq!(created.proposal.evidence_message_ids, vec![evidence]);

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
                operator: None,
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
    assert_eq!(created.proposal.evidence_message_ids, vec![first, second]);
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
            global_cooldown: Duration::zero(),
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
            global_cooldown: Duration::zero(),
            field_cooldown: Duration::days(7),
            max_pending: 10,
        },
    );

    let mut first = draft(vec![evidence.clone()]);
    first.field_key = "personality".to_string();
    let first = service.create_proposal(first, at(0)).unwrap();
    service
        .decide(
            &first.proposal.id,
            ProposalStatus::Rejected,
            false,
            "test",
            at(1),
        )
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
        .decide(
            &created.proposal.id,
            ProposalStatus::Rejected,
            false,
            "test",
            at(1),
        )
        .unwrap();
    assert_eq!(rejected.status, "rejected");
    assert!(
        service
            .decide(
                &created.proposal.id,
                ProposalStatus::Approved,
                false,
                "test",
                at(2),
            )
            .is_err(),
        "已决提案不能再次审批"
    );
    assert!(
        service
            .decide(
                &created.proposal.id,
                ProposalStatus::Applied,
                false,
                "test",
                at(3),
            )
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
            .decide(
                &created.proposal.id,
                ProposalStatus::Approved,
                false,
                "test",
                at(1),
            )
            .is_err(),
        "关系姿态缺少明确同意时不能批准"
    );
    let approved = service
        .decide(
            &created.proposal.id,
            ProposalStatus::Approved,
            true,
            "test",
            at(2),
        )
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
    assert_eq!(created.proposal.evidence_message_ids, vec![evidence]);
    assert_eq!(created.proposal.status, "pending");
    assert_eq!(created.proposal.risk_level, "medium");
}

#[test]
fn proposal_limits_have_mvp_defaults() {
    let limits = ProposalLimits::default();
    assert_eq!(limits.global_cooldown, Duration::days(1));
    assert_eq!(limits.field_cooldown, Duration::days(7));
    assert_eq!(limits.max_pending, 3);
}

#[test]
fn approve_and_apply_updates_profile_and_invalidates_cache_immediately() {
    let db = db();
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    let service = IdentityService::new(db.clone(), profile.clone());
    let evidence = add_message(&db, "user", "我喜欢短一点的回复");
    let created = service
        .create_proposal(draft(vec![evidence]), at(0))
        .unwrap();

    assert_eq!(profile.snapshot().text, "");
    let applied = service
        .approve_and_apply(&created.proposal.id, false, "test", at(10))
        .unwrap();
    assert_eq!(applied.version, 1);
    assert_eq!(
        profile.snapshot(),
        crate::core::identity::profile::ProfileSnapshot {
            version: Some(1),
            text: "表达风格：更愿意主动分享自己的看法，但仍然保持短句".to_string(),
        },
        "提交后应立即失效缓存，而不是等 TTL"
    );
    assert_eq!(service.get(&created.proposal.id).unwrap().status, "applied");
    assert_eq!(
        service
            .get(&created.proposal.id)
            .unwrap()
            .decided_by
            .as_deref(),
        Some("test"),
        "审批人必须落入提案审计"
    );
}

#[test]
fn approve_and_apply_rejects_relationship_stance_without_explicit_consent() {
    let db = db();
    let evidence = add_message(&db, "user", "我们可以更亲近一点");
    let service = service(db);
    let mut draft = draft(vec![evidence]);
    draft.field_key = "relationship_stance".to_string();
    draft.after_value = "更亲近，但尊重独处".to_string();
    let created = service.create_proposal(draft, at(0)).unwrap();

    assert!(
        service
            .approve_and_apply(&created.proposal.id, false, "test", at(10))
            .is_err(),
        "关系姿态缺少明确同意时不能应用"
    );
    assert_eq!(service.get(&created.proposal.id).unwrap().status, "pending");
    let applied = service
        .approve_and_apply(&created.proposal.id, true, "test", at(11))
        .unwrap();
    assert_eq!(applied.version, 1);
}

#[test]
fn reject_changes_only_proposal_status() {
    let db = db();
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    let service = IdentityService::new(db.clone(), profile.clone());
    let evidence = add_message(&db, "user", "先不用改");
    let created = service
        .create_proposal(draft(vec![evidence]), at(0))
        .unwrap();
    profile.snapshot();

    let rejected = service.reject(&created.proposal.id, "test", at(1)).unwrap();
    assert_eq!(rejected.status, "rejected");
    assert_eq!(profile.snapshot().text, "");
    assert!(
        crate::db::identity::latest_version(&crate::db::lock_db(&db))
            .unwrap()
            .is_none()
    );
}

#[test]
fn rollback_service_restores_profile_and_invalidates_cache() {
    let db = db();
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    let service = IdentityService::new(db.clone(), profile.clone());
    let evidence = add_message(&db, "user", "喜欢你短一点");
    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "语气", "旧语气").unwrap();
    }
    assert_eq!(profile.snapshot().text, "语气：旧语气");

    let created = service
        .create_proposal(draft(vec![evidence]), at(0))
        .unwrap();
    service
        .approve_and_apply(&created.proposal.id, false, "test", at(10))
        .unwrap();
    assert_eq!(
        profile.snapshot().text,
        "表达风格：更愿意主动分享自己的看法，但仍然保持短句"
    );

    let rollback = service.rollback("test", at(20)).unwrap();
    assert_eq!(rollback.source, "rollback");
    assert_eq!(rollback.operator.as_deref(), Some("test"));
    assert_eq!(profile.snapshot().text, "表达风格：旧语气");
    assert_eq!(
        service.get(&created.proposal.id).unwrap().status,
        "rolled_back"
    );
}

#[test]
fn rollback_version_restores_requested_snapshot_and_invalidates_cache() {
    let db = db();
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    let service = IdentityService::new(db.clone(), profile.clone());
    {
        let guard = crate::db::lock_db(&db);
        crate::db::identity::insert_version(
            &guard,
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
        crate::db::identity::insert_version(
            &guard,
            &IdentityVersion {
                version: 2,
                snapshot: r#"{"personality":"慢热","voice_style":"新语气"}"#.to_string(),
                proposal_id: None,
                source: "manual".to_string(),
                reason: "后续版本".to_string(),
                created_at: at(1),
                operator: None,
                rolled_back_from: None,
            },
        )
        .unwrap();
    }
    assert_eq!(profile.snapshot().text, "性格：慢热；表达风格：新语气");

    let restored = service.rollback_version(Some(1), "test", at(20)).unwrap();
    assert_eq!(restored.version, 3);
    assert_eq!(restored.rolled_back_from, Some(2));
    assert_eq!(profile.snapshot().text, "性格：慢热；表达风格：旧语气");
}

#[test]
fn global_daily_cooldown_blocks_second_proposal_and_gate_reports_reason() {
    let db = db();
    let evidence = add_message(&db, "user", "证据");
    let service = service(db);
    let mut first = draft(vec![evidence.clone()]);
    first.field_key = "interests".to_string();
    service.create_proposal(first, at(0)).unwrap();

    let mut second = draft(vec![evidence]);
    second.field_key = "voice_style".to_string();
    let err = service
        .create_proposal(second, at(60))
        .unwrap_err()
        .to_string();
    assert!(err.contains("每天最多运行一次"), "{err}");
    assert_eq!(
        service.evolution_gate(at(60)).unwrap(),
        EvolutionGate::GlobalCooldown
    );
    assert_eq!(
        service.evolution_gate(at(86_400)).unwrap(),
        EvolutionGate::Run
    );
}

#[test]
fn evidence_snapshot_collects_current_identity_messages_and_memories() {
    let db = db();
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    let service = IdentityService::new(db.clone(), profile);
    let sessions = Arc::new(crate::core::session::Sessions::new(db.clone()).unwrap());
    let session_id = sessions.resolve("telegram", "100").unwrap();
    let stored = sessions
        .add_message(&session_id, "user", "我喜欢你有自己的看法")
        .unwrap();
    {
        let guard = crate::db::lock_db(&db);
        crate::db::self_profile::set(guard.conn(), "性格", "慢热").unwrap();
        crate::db::memory::create_record(
            &guard,
            &crate::db::memory::Memory::new(
                "用户喜欢有观点的交流".to_string(),
                crate::db::memory::MemoryKind::Preference,
                Some(stored.id.clone()),
                None,
            ),
        )
        .unwrap();
    }

    let snapshot = service.evidence_snapshot(&sessions, 10, 10).unwrap();
    assert_eq!(snapshot.identity, "性格：慢热");
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].id, stored.id);
    assert_eq!(snapshot.memories.len(), 1);
    assert_eq!(
        snapshot.memories[0].source_message_id.as_deref(),
        Some(stored.id.as_str())
    );
}

#[test]
fn approve_and_apply_failure_leaves_profile_and_cache_untouched() {
    let db = db();
    let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
    let service = IdentityService::new(db.clone(), profile.clone());
    let evidence = add_message(&db, "user", "证据");
    let created = service
        .create_proposal(draft(vec![evidence]), at(0))
        .unwrap();
    assert_eq!(profile.snapshot().text, "");

    let guard = crate::db::lock_db(&db);
    crate::db::identity::insert_version(
        &guard,
        &IdentityVersion {
            version: 1,
            snapshot: r#"{"personality":"并发修改"}"#.to_string(),
            proposal_id: None,
            source: "manual".to_string(),
            reason: "并发变更".to_string(),
            created_at: at(5),
            operator: None,
            rolled_back_from: None,
        },
    )
    .unwrap();
    drop(guard);

    assert!(
        service
            .approve_and_apply(&created.proposal.id, false, "test", at(10))
            .is_err(),
        "基线版本漂移必须拒绝应用"
    );
    assert_eq!(service.get(&created.proposal.id).unwrap().status, "pending");
    assert_eq!(
        profile.snapshot().text,
        "",
        "失败不应清空已有缓存，也不应写入新身份"
    );
}
