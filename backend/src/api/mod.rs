pub mod chat;
pub mod memory;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;

use crate::Assistant;

/// HTTP 访问日志中间件：记录 method / uri / status / 耗时。
/// 注意：SSE/WS 等流式响应的耗时只在首个响应（头）返回时结束。
async fn access_log(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    let resp = next.run(req).await;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let status = resp.status();
    if status.is_server_error() {
        tracing::error!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms,
            "http request error"
        );
    } else {
        tracing::info!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms,
            "http request"
        );
    }
    resp
}

pub fn build_router(state: Arc<Assistant>) -> Router {
    Router::new()
        .merge(chat::routes(state.clone()))
        .merge(memory::routes(state))
        .route_layer(axum::middleware::from_fn(access_log))
}