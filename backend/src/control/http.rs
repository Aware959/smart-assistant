//! 控制接口路由与 DTO。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::identity::evolution::IdentityEvolutionAgent;
use crate::core::identity::profile::Profile;
use crate::core::identity::service::{IdentityService, ProposalView};
use crate::core::session::Sessions;
use crate::core::settings::{SettingsAuditEntry, SettingsService, SettingsView};
use crate::error::SqlError;
use crate::utils::version;
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex as TokioMutex;

use super::auth::require_token;

pub(crate) struct AppState {
    pub(crate) settings: Arc<SettingsService>,
    pub(crate) identity: Arc<IdentityService>,
    pub(crate) evolution: Arc<IdentityEvolutionAgent>,
    pub(crate) sessions: Arc<Sessions>,
    pub(crate) evolve_lock: Arc<TokioMutex<()>>,
    pub(crate) token: String,
    pub(crate) started: Instant,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

impl From<SqlError> for ApiError {
    fn from(error: SqlError) -> Self {
        match error {
            SqlError::NotFound(message) => Self::new(StatusCode::NOT_FOUND, message),
            SqlError::Conflict(message) => Self::new(StatusCode::CONFLICT, message),
            SqlError::Config(message) => Self::new(StatusCode::UNPROCESSABLE_ENTITY, message),
            other => Self::new(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    status: &'static str,
    pid: u32,
    version: String,
    revision: i64,
    uptime_seconds: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlStateFile {
    pid: u32,
    address: String,
    version: String,
    started_at: String,
    token_file: String,
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Deserialize)]
struct ValidateRequest {
    changes: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest {
    expected_revision: i64,
    #[serde(default)]
    operator: Option<String>,
    changes: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityOverview {
    version: Option<u64>,
    text: String,
    latest_check_at: Option<String>,
    gate: String,
    pending_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProposalDecisionRequest {
    #[serde(default = "default_operator")]
    operator: Option<String>,
    #[serde(default)]
    explicit_consent: bool,
}

fn default_operator() -> Option<String> {
    Some("desktop".to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollbackRequest {
    #[serde(default = "default_operator")]
    operator: Option<String>,
    #[serde(default)]
    version: Option<i64>,
}

/// 控制服务启动参数，避免 `serve` 参数过多触发 clippy 警告。
pub struct ServeParams {
    pub settings: Arc<SettingsService>,
    pub db: Arc<StdMutex<crate::db::Database>>,
    pub sessions: Arc<Sessions>,
    pub evolution: Arc<IdentityEvolutionAgent>,
    pub enabled: bool,
    pub addr: String,
    pub state_file: String,
    pub token_file: String,
}

/// 启动控制服务，写状态与 token 文件，然后阻塞到进程退出。
pub async fn serve(params: ServeParams) -> std::io::Result<()> {
    let ServeParams {
        settings,
        db,
        sessions,
        evolution,
        enabled,
        addr,
        state_file,
        token_file,
    } = params;
    if !enabled {
        tracing::info!("本地控制接口已关闭（CONTROL_ENABLED=0）");
        return Ok(());
    }
    let addr: SocketAddr = addr
        .parse()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "CONTROL_ADDR 必须绑定 loopback，不能暴露到局域网",
        ));
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let token_path = PathBuf::from(token_file);
    write_private_file(&token_path, token.as_bytes())?;
    let profile = Arc::new(Profile::new(db.clone()));
    let identity = Arc::new(IdentityService::new(db, profile));
    let state = Arc::new(AppState {
        settings,
        identity,
        evolution,
        sessions,
        evolve_lock: Arc::new(TokioMutex::new(())),
        token,
        started: Instant::now(),
    });
    let app = control_router(state.clone());
    let state_file = PathBuf::from(state_file);
    write_state_file(&state_file, bound, &token_path)?;
    tracing::info!(addr = %bound, "本地控制接口已启动");
    axum::serve(listener, app).await
}

/// 控制接口路由表。单独抽出，便于测试覆盖全部路径（axum 0.8 要求 `{param}`，
/// 旧的 `:param` 写法会在构造路由时 panic）。
fn control_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/control/v1/health", get(health))
        .route(
            "/control/v1/settings",
            get(settings_view).patch(update_settings),
        )
        .route("/control/v1/settings/schema", get(settings_schema))
        .route("/control/v1/settings/audit", get(settings_audit))
        .route("/control/v1/settings/validate", post(validate_settings))
        .route("/control/v1/identity", get(identity_overview))
        .route("/control/v1/identity/proposals", get(list_proposals))
        .route(
            "/control/v1/identity/proposals/{id}/approve",
            post(approve_proposal),
        )
        .route(
            "/control/v1/identity/proposals/{id}/reject",
            post(reject_proposal),
        )
        .route("/control/v1/identity/rollback", post(rollback_identity))
        .route("/control/v1/identity/evolve", post(trigger_evolution))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Health> {
    Json(Health {
        status: "ok",
        pid: std::process::id(),
        version: version::display(),
        revision: state.settings.current().revision,
        uptime_seconds: state.started.elapsed().as_secs(),
    })
}

async fn settings_view(State(state): State<Arc<AppState>>) -> Json<SettingsView> {
    Json(state.settings.current())
}

async fn settings_schema(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<crate::core::settings::SettingsSchemaItem>> {
    Json(state.settings.schema())
}

async fn settings_audit(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<SettingsAuditEntry>>, ApiError> {
    let limit = query.limit.clamp(1, 200);
    Ok(Json(state.settings.audit(limit)?))
}

async fn validate_settings(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ValidateRequest>,
) -> Result<Json<BTreeMap<String, Value>>, ApiError> {
    Ok(Json(state.settings.validate(request.changes)?))
}

async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<crate::core::settings::SettingsUpdate>, ApiError> {
    let operator = request
        .operator
        .unwrap_or_else(|| "control-api".to_string());
    Ok(Json(state.settings.update(
        request.changes,
        request.expected_revision,
        &operator,
        Utc::now(),
    )?))
}

async fn identity_overview(
    State(state): State<Arc<AppState>>,
) -> Result<Json<IdentityOverview>, ApiError> {
    let identity_view = state.identity.current();
    let now = Utc::now();
    let gate = match state.identity.evolution_gate(now)? {
        crate::core::identity::service::EvolutionGate::Run => "run",
        crate::core::identity::service::EvolutionGate::PendingLimit => "pending_limit",
        crate::core::identity::service::EvolutionGate::GlobalCooldown => "global_cooldown",
        crate::core::identity::service::EvolutionGate::Disabled => "disabled",
    };
    let latest_check_at = state
        .identity
        .latest_evolution_check()?
        .map(|t| t.to_rfc3339());
    let pending = state.identity.pending(3)?;
    Ok(Json(IdentityOverview {
        version: identity_view.version,
        text: identity_view.text,
        latest_check_at,
        gate: gate.to_string(),
        pending_count: pending.len(),
    }))
}

async fn list_proposals(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<ProposalView>>, ApiError> {
    let limit = query.limit.clamp(1, 200);
    Ok(Json(state.identity.pending(limit)?))
}

async fn approve_proposal(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ProposalDecisionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let operator = request.operator.unwrap_or_else(|| "desktop".to_string());
    let result =
        state
            .identity
            .approve_and_apply(&id, request.explicit_consent, &operator, Utc::now())?;
    Ok(Json(serde_json::json!({
        "version": result.version,
        "proposalId": result.proposal_id,
        "source": result.source,
    })))
}

async fn reject_proposal(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ProposalDecisionRequest>,
) -> Result<Json<ProposalView>, ApiError> {
    let operator = request.operator.unwrap_or_else(|| "desktop".to_string());
    let result = state.identity.reject(&id, &operator, Utc::now())?;
    Ok(Json(result))
}

async fn rollback_identity(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RollbackRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let operator = request.operator.unwrap_or_else(|| "desktop".to_string());
    let result = state
        .identity
        .rollback_version(request.version, &operator, Utc::now())?;
    Ok(Json(serde_json::json!({
        "version": result.version,
        "proposalId": result.proposal_id,
        "source": result.source,
        "reason": result.reason,
        "rolledBackFrom": result.rolled_back_from,
    })))
}

async fn trigger_evolution(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _guard = state
        .evolve_lock
        .try_lock()
        .map_err(|_| ApiError::new(StatusCode::CONFLICT, "上一次手动演化仍在运行，请稍后再试"))?;
    let now = Utc::now();
    let gate = state.identity.evolution_gate(now)?;
    if gate != crate::core::identity::service::EvolutionGate::Run {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("演化被限流挡住（{gate:?}），请稍后再试"),
        ));
    }
    state.identity.mark_evolution_checked(now)?;
    let agent = state.evolution.clone();
    let sessions = state.sessions.clone();
    let result = tokio::task::spawn_blocking(move || agent.run_once(&sessions, now))
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("演化任务崩溃：{error}"),
            )
        })?
        .map_err(ApiError::from)?;
    match result {
        Some(created) => Ok(Json(serde_json::json!({
            "proposalId": created.proposal.id,
            "fieldKey": created.proposal.field_key,
            "riskLevel": created.proposal.risk_level,
        }))),
        None => Ok(Json(serde_json::json!({ "proposalId": null }))),
    }
}

fn write_state_file(path: &Path, addr: SocketAddr, token_file: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let state = ControlStateFile {
        pid: std::process::id(),
        address: format!("http://{addr}"),
        version: version::display(),
        started_at: Utc::now().to_rfc3339(),
        token_file: token_file.display().to_string(),
    };
    std::fs::write(path, serde_json::to_vec_pretty(&state)?)
}

fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::SettingsService;
    use crate::db::Database;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::sync::Mutex;
    use tower::ServiceExt;

    fn state() -> Arc<AppState> {
        let db = Arc::new(Mutex::new(Database::in_memory().unwrap()));
        let settings = Arc::new(SettingsService::new(db.clone()).unwrap());
        let profile = Arc::new(crate::core::identity::profile::Profile::new(db.clone()));
        let identity = Arc::new(IdentityService::new(db.clone(), profile));
        let sessions = Arc::new(Sessions::new(db).unwrap());
        let llm = Arc::new(crate::llm::chat::Llm);
        let evolution_config = crate::core::identity::evolution::EvolutionConfig::enabled(
            crate::core::identity::evolution::RUNTIME_MODEL.to_string(),
        );
        let evolution = Arc::new(IdentityEvolutionAgent::new(
            evolution_config,
            llm,
            identity.clone(),
        ));
        Arc::new(AppState {
            settings,
            identity,
            evolution,
            sessions,
            evolve_lock: Arc::new(TokioMutex::new(())),
            token: "test-token".to_string(),
            started: Instant::now(),
        })
    }

    fn router(state: Arc<AppState>) -> Router {
        control_router(state)
    }

    #[tokio::test]
    async fn identity_proposal_routes_accept_path_parameters() {
        // axum 0.8 的路径参数是 `{id}`；写回 `:id` 会在构造路由时 panic。
        let app = router(state());
        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/v1/identity/proposals/missing-id/approve")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"operator": "test", "explicitConsent": true})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 路由命中：方法不匹配才是 405，未命中才是 404。
        assert_ne!(post.status(), StatusCode::METHOD_NOT_ALLOWED);

        let get = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/control/v1/identity/proposals/some-id/reject")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn rejects_missing_or_wrong_token() {
        let app = router(state());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/control/v1/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/control/v1/settings")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn patch_requires_current_revision_and_returns_conflict() {
        let app = router(state());
        let request = || {
            Request::builder()
                .method("PATCH")
                .uri("/control/v1/settings")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "expectedRevision": 0,
                        "operator": "test",
                        "changes": {"intent_enabled": true}
                    })
                    .to_string(),
                ))
                .unwrap()
        };
        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = app.oneshot(request()).await.unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
        let body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("revision"));
    }
}
