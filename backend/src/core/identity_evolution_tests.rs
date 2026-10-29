//! 身份演化 Agent 测试：配置、提示词、严格解析、落库与权限边界。

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use super::*;
use crate::core::agent_role::AgentRole;
use crate::db::Database;

struct FakeLlm {
    response: FakeResponse,
    calls: Arc<Mutex<Vec<Call>>>,
}

#[derive(Clone)]
enum FakeResponse {
    Ok(String),
    Err(String),
}

#[derive(Debug, Clone)]
struct Call {
    messages: Vec<ChatMessage>,
    model: String,
    name: String,
    schema: serde_json::Value,
}

impl FakeLlm {
    fn returning(response: impl Into<String>) -> Self {
        Self {
            response: FakeResponse::Ok(response.into()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            response: FakeResponse::Err(message.to_string()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl Clone for FakeLlm {
    fn clone(&self) -> Self {
        Self {
            response: self.response.clone(),
            calls: self.calls.clone(),
        }
    }
}

impl StructuredLlm for FakeLlm {
    fn complete_structured(
        &self,
        messages: &[ChatMessage],
        model: &str,
        name: &str,
        schema: serde_json::Value,
    ) -> Result<String> {
        self.calls.lock().unwrap().push(Call {
            messages: messages.to_vec(),
            model: model.to_string(),
            name: name.to_string(),
            schema,
        });
        match &self.response {
            FakeResponse::Ok(value) => Ok(value.clone()),
            FakeResponse::Err(message) => Err(SqlError::Inference(message.clone())),
        }
    }
}

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::seconds(secs)
}

fn setup() -> (Arc<Mutex<Database>>, String, EvidenceSnapshot) {
    let db = Arc::new(Mutex::new(Database::in_memory().unwrap()));
    let message_id = {
        let guard = crate::db::lock_db(&db);
        let session = crate::db::session::create(&guard, "test").unwrap();
        crate::db::message::create(&guard, &session.id, "user", "我喜欢你有自己的看法")
            .unwrap()
            .id
    };
    let snapshot = EvidenceSnapshot {
        identity: "表达风格：简短".to_string(),
        messages: vec![EvidenceMessage {
            id: message_id.clone(),
            role: "user".to_string(),
            content: "我喜欢你有自己的看法".to_string(),
        }],
        memories: vec![EvidenceMemory {
            content: "用户偏好有观点的交流".to_string(),
            source_message_id: Some(message_id.clone()),
        }],
    };
    (db, message_id, snapshot)
}

fn agent(
    db: Arc<Mutex<Database>>,
    config: EvolutionConfig,
    llm: FakeLlm,
) -> IdentityEvolutionAgent {
    let profile = Arc::new(crate::core::self_profile::Profile::new(db.clone()));
    let identity = Arc::new(IdentityService::new(db, profile));
    IdentityEvolutionAgent::new(config, Arc::new(llm), identity)
}

fn proposal_json(message_id: &str) -> String {
    serde_json::json!({
        "field_key": "voice_style",
        "after_value": "更愿意主动分享自己的看法，但仍然保持短句",
        "reason": "用户明确表示喜欢有观点的回复",
        "evidence_message_ids": [message_id],
        "confidence": "medium"
    })
    .to_string()
}

fn no_proposal_json() -> String {
    serde_json::json!({
        "field_key": null,
        "after_value": null,
        "reason": null,
        "evidence_message_ids": [],
        "confidence": null
    })
    .to_string()
}

#[test]
fn config_is_disabled_by_default_and_disabled_run_skips_llm() {
    assert_eq!(
        EvolutionConfig::default(),
        EvolutionConfig {
            enabled: false,
            model: String::new()
        }
    );
    let (db, _id, snapshot) = setup();
    let llm = FakeLlm::returning(proposal_json("unused"));
    let agent = agent(db, EvolutionConfig::disabled(), llm.clone());
    assert!(agent.run(&snapshot, at(0)).unwrap().is_none());
    assert!(llm.calls().is_empty(), "关闭时不得调用 LLM");
}

#[test]
fn creates_one_pending_proposal_without_other_side_effects() {
    let (db, message_id, snapshot) = setup();
    let response = proposal_json(&message_id);
    let llm = FakeLlm::returning(response);
    let agent = agent(
        db.clone(),
        EvolutionConfig::enabled("test-model"),
        llm.clone(),
    );

    let before_messages: i64 = {
        let guard = crate::db::lock_db(&db);
        guard
            .conn()
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap()
    };
    let created = agent.run(&snapshot, at(0)).unwrap().unwrap();

    assert_eq!(created.proposal.status, "pending");
    assert_eq!(created.proposal.field_key, "voice_style");
    assert_eq!(created.proposal.proposer, PROPOSER);
    assert_eq!(created.proposal.risk_level, "medium");
    assert_eq!(
        created.proposal.evidence_message_ids,
        vec![message_id.clone()]
    );

    let guard = crate::db::lock_db(&db);
    let proposals = crate::db::identity::list_proposals_by_status(&guard, "pending", 10).unwrap();
    assert_eq!(proposals.len(), 1, "单次运行最多创建一条提案");
    assert_eq!(
        crate::db::identity::latest_version(&guard).unwrap(),
        None,
        "不得直接应用身份变更"
    );
    assert!(
        crate::db::self_profile::all(guard.conn())
            .unwrap()
            .is_empty(),
        "不得修改当前身份"
    );
    let after_messages: i64 = guard
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let memories: i64 = guard
        .conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after_messages, before_messages, "不得发送/写入消息");
    assert_eq!(memories, 0, "不得写用户记忆");

    let calls = llm.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].model, "test-model");
    assert_eq!(calls[0].name, SCHEMA_NAME);
    assert_eq!(
        calls[0].schema["properties"].as_object().unwrap().len(),
        5,
        "运行时应把严格五字段 schema 交给结构化输出"
    );
    let prompt = calls[0]
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains(&message_id));
    assert!(prompt.contains("用户偏好有观点的交流"));
    assert!(prompt.contains("untrusted evidence"));
}

#[test]
fn schema_has_exact_t5_compatible_fields_and_no_model_reported_risk() {
    let schema = proposal_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["required"],
        serde_json::json!([
            "field_key",
            "after_value",
            "reason",
            "evidence_message_ids",
            "confidence"
        ])
    );
    let props = schema["properties"].as_object().unwrap();
    assert_eq!(props.len(), 5);
    assert!(!props.contains_key("before"));
    assert!(!props.contains_key("before_value"));
    assert!(!props.contains_key("risk"));
    assert!(!props.contains_key("risk_level"));
}

#[test]
fn evidence_ids_are_deduplicated_before_persistence() {
    let (db, message_id, snapshot) = setup();
    let response = serde_json::json!({
        "field_key": "voice_style",
        "after_value": "更短，但保留自己的看法",
        "reason": "同一条证据被重复引用",
        "evidence_message_ids": [message_id, message_id],
        "confidence": "low"
    })
    .to_string();
    let llm = FakeLlm::returning(response);
    let agent = agent(db, EvolutionConfig::enabled("test-model"), llm);
    let created = agent.run(&snapshot, at(0)).unwrap().unwrap();
    assert_eq!(created.proposal.evidence_message_ids, vec![message_id]);
}

#[test]
fn insufficient_evidence_returns_none_without_writing_a_proposal() {
    let (db, _id, snapshot) = setup();
    let llm = FakeLlm::returning(no_proposal_json());
    let agent = agent(db.clone(), EvolutionConfig::enabled("test-model"), llm);
    assert!(agent.run(&snapshot, at(0)).unwrap().is_none());
    let guard = crate::db::lock_db(&db);
    assert!(
        crate::db::identity::list_proposals_by_status(&guard, "pending", 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn empty_snapshot_returns_none_without_calling_llm() {
    let (db, _id, _snapshot) = setup();
    let llm = FakeLlm::returning(no_proposal_json());
    let agent = agent(db, EvolutionConfig::enabled("test-model"), llm.clone());
    assert!(agent
        .run(&EvidenceSnapshot::default(), at(0))
        .unwrap()
        .is_none());
    assert!(llm.calls().is_empty());
}

#[test]
fn hallucinated_or_empty_evidence_is_rejected() {
    let (db, _id, snapshot) = setup();
    let llm = FakeLlm::returning(proposal_json("missing"));
    let evolution = agent(db.clone(), EvolutionConfig::enabled("test-model"), llm);
    let err = evolution.run(&snapshot, at(0)).unwrap_err().to_string();
    assert!(err.contains("证据快照中不存在"), "{err}");

    let (db, _id, snapshot) = setup();
    let llm = FakeLlm::returning(proposal_json(""));
    let evolution = agent(db.clone(), EvolutionConfig::enabled("test-model"), llm);
    let err = evolution.run(&snapshot, at(0)).unwrap_err().to_string();
    assert!(err.contains("空证据 id"), "{err}");

    let guard = crate::db::lock_db(&db);
    assert!(
        crate::db::identity::list_proposals_by_status(&guard, "pending", 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn malformed_partial_or_unknown_json_is_rejected_without_side_effects() {
    for response in [
        "not-json".to_string(),
        serde_json::json!({
            "field_key": "voice_style",
            "after_value": "更短",
            "reason": "证据",
            "evidence_message_ids": [],
            "confidence": null,
            "before_value": "模型不得自报"
        })
        .to_string(),
    ] {
        let (db, message_id, snapshot) = setup();
        let llm = FakeLlm::returning(response);
        let agent = agent(db.clone(), EvolutionConfig::enabled("test-model"), llm);
        let result = agent.run(&snapshot, at(0));
        if result.as_ref().is_ok_and(Option::is_none) {
            assert_eq!(message_id, snapshot.messages[0].id);
            continue;
        }
        assert!(result.is_err());
        let guard = crate::db::lock_db(&db);
        assert!(
            crate::db::identity::list_proposals_by_status(&guard, "pending", 10)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn enabled_without_model_fails_before_calling_llm() {
    let (db, _id, snapshot) = setup();
    let llm = FakeLlm::returning(no_proposal_json());
    let agent = agent(
        db,
        EvolutionConfig {
            enabled: true,
            model: "  ".to_string(),
        },
        llm.clone(),
    );
    assert!(agent.run(&snapshot, at(0)).is_err());
    assert!(llm.calls().is_empty());
}

#[test]
fn identity_role_has_no_send_write_or_apply_capability() {
    let role = AgentRole::IdentityEvolution;
    assert!(!role.can_message_user());
    assert!(!role.can_write_user_memory());
    assert!(!role.can_apply_identity_change());
}

#[test]
fn llm_failure_is_propagated_without_writing() {
    let (db, _id, snapshot) = setup();
    let failing = FakeLlm::failing("network down");
    let agent = agent(db.clone(), EvolutionConfig::enabled("test-model"), failing);
    assert!(agent.run(&snapshot, at(0)).is_err());
    let guard = crate::db::lock_db(&db);
    assert!(
        crate::db::identity::list_proposals_by_status(&guard, "pending", 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn production_run_once_collects_evidence_and_respects_daily_gate() {
    let db = Arc::new(Mutex::new(Database::in_memory().unwrap()));
    let sessions = Arc::new(crate::core::session::Sessions::new(db.clone()).unwrap());
    let session_id = sessions.resolve("telegram", "100").unwrap();
    let message_id = sessions
        .add_message(&session_id, "user", "我喜欢你有自己的看法")
        .unwrap()
        .id;
    let profile = Arc::new(crate::core::self_profile::Profile::new(db.clone()));
    let identity = Arc::new(IdentityService::new(db.clone(), profile));
    let llm = FakeLlm::returning(proposal_json(&message_id));
    let evolution = IdentityEvolutionAgent::new(
        EvolutionConfig::enabled("test-model"),
        Arc::new(llm.clone()),
        identity.clone(),
    );

    let created = evolution.run_once(&sessions, at(0)).unwrap().unwrap();
    assert_eq!(created.proposal.status, "pending");
    assert_eq!(llm.calls().len(), 1);
    assert!(
        evolution.run_once(&sessions, at(60)).unwrap().is_none(),
        "全局每日冷却内不应再次调用模型"
    );
    assert_eq!(llm.calls().len(), 1);
}
