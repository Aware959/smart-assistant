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

use crate::core::settings::{SettingsAuditEntry, SettingsService, SettingsView};
use crate::error::SqlError;
use crate::utils::version;

use super::auth::require_token;

pub(crate) struct AppState {
    pub(crate) settings: Arc<SettingsService>,
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

/// 启动控制服务，写状态与 token 文件，然后阻塞到进程退出。
pub async fn serve(
    settings: Arc<SettingsService>,
    enabled: bool,
    addr: &str,
    state_file: &str,
    token_file: &str,
) -> std::io::Result<()> {
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
    let state = Arc::new(AppState {
        settings,
        token,
        started: Instant::now(),
    });
    let app = Router::new()
        .route("/control/v1/health", get(health))
        .route(
            "/control/v1/settings",
            get(settings_view).patch(update_settings),
        )
        .route("/control/v1/settings/schema", get(settings_schema))
        .route("/control/v1/settings/audit", get(settings_audit))
        .route("/control/v1/settings/validate", post(validate_settings))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state);
    let state_file = PathBuf::from(state_file);
    write_state_file(&state_file, bound, &token_path)?;
    tracing::info!(addr = %bound, "本地控制接口已启动");
    axum::serve(listener, app).await
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
        let settings = Arc::new(SettingsService::new(db).unwrap());
        Arc::new(AppState {
            settings,
            token: "test-token".to_string(),
            started: Instant::now(),
        })
    }

    fn router(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/control/v1/settings",
                get(settings_view).patch(update_settings),
            )
            .route("/control/v1/health", get(health))
            .layer(middleware::from_fn_with_state(state.clone(), require_token))
            .with_state(state)
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
