use std::sync::Arc;

use axum::{
    extract::State,
    http::{header::AUTHORIZATION, HeaderMap},
    middleware::Next,
    response::Response,
};

use super::http::{ApiError, AppState};

/// Bearer token 校验中间件。
pub(crate) async fn require_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if token != Some(state.token.as_str()) {
        return Err(ApiError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "控制接口 token 无效",
        ));
    }
    Ok(next.run(request).await)
}
