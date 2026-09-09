use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{State, WebSocketUpgrade},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures::channel::mpsc;
use futures::StreamExt;
use serde::Deserialize;

use crate::Assistant;
use crate::ChatInput;
use crate::ChatTurn;

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    /// 会话 id；留空后端自动新建。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 显式携带的历史轮次（可选）；缺省用后端自动构建的会话上下文。
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// 自动构建上下文用最近几条历史消息；缺省 6。
    #[serde(default)]
    pub history_count: Option<u32>,
}

pub fn routes(state: Arc<Assistant>) -> Router {
    Router::new()
        .route("/chat/stream", post(handle_chat_stream))
        .route("/ws", get(handle_ws))
        .with_state(state)
}

// ---------- SSE 实时输出 ----------

enum SseMsg {
    Delta(String),
    Done(Box<Result<crate::ChatOutput, String>>),
}

/// 生成 OpenAI 兼容的 `chat.completion.chunk` 数据行。
fn openai_chunk_json(id: &str, created: u64, model: &str, delta_content: &str, finish_reason: Option<&str>) -> String {
    let delta = if finish_reason.is_some() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "role": "assistant", "content": delta_content })
    };
    serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    })
    .to_string()
}

fn sse_stream_meta() -> (String, u64, String) {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let model = crate::config::Config::get().llm_model.clone();
    (id, created, model)
}

/// POST /chat/stream：对话实时流，回复以 OpenAI 兼容 SSE 推送。
///
/// - 普通数据行均为 `chat.completion.chunk`（`data: {...}`），结尾为 `data: [DONE]`；
/// - 额外的元数据事件 `event: done`：`data: <ChatOutput JSON>`（含 session_id / memory / entities / relations）。
async fn handle_chat_stream(
    State(state): State<Arc<Assistant>>,
    Json(req): Json<ChatRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let input = ChatInput {
        message: req.message,
        session_id: req.session_id,
        history: req.history,
        history_count: req.history_count,
    };
    let (id, created, model) = sse_stream_meta();

    // futures 的 mpsc 是同步发送（无 runtime 上下文检查），可在阻塞线程池里安全使用；
    // tokio 的 blocking_send 在 runtime 已 enter 的阻塞线程上会 panic，故不用。
    let (tx, rx) = mpsc::unbounded::<SseMsg>();

    // LLM 生成在阻塞线程池跑（内部已自带 runtime block_on），增量经 channel 回流。
    tokio::task::spawn_blocking(move || {
        let outcome = state
            .chat_stream(&input, |delta| {
                let _ = tx.unbounded_send(SseMsg::Delta(delta.to_string()));
            })
            .map_err(|e| e.to_string());
        let _ = tx.unbounded_send(SseMsg::Done(Box::new(outcome)));
    });

    let stream = rx.flat_map(move |msg| match msg {
        SseMsg::Delta(text) => {
            let chunk = openai_chunk_json(&id, created, &model, &text, None);
            futures::stream::iter(vec![Ok::<_, Infallible>(Event::default().data(chunk))])
        }
        SseMsg::Done(outcome) => {
            let mut events: Vec<Result<Event, Infallible>> = Vec::new();
            let finish = openai_chunk_json(&id, created, &model, "", Some("stop"));
            events.push(Ok(Event::default().data(finish)));
            match *outcome {
                Ok(out) => {
                    events.push(Ok(Event::default().data("[DONE]")));
                    let meta = serde_json::to_string(&out).unwrap_or_else(|_| "{}".into());
                    events.push(Ok(Event::default().event("done").data(meta)));
                }
                Err(message) => {
                    let err = serde_json::json!({
                        "error": { "message": message, "type": "assistant_stream_error" }
                    })
                    .to_string();
                    events.push(Ok(Event::default().data(err)));
                    events.push(Ok(Event::default().data("[DONE]")));
                    events.push(Ok(Event::default().event("done").data(
                        serde_json::json!({ "type": "error", "message": message }).to_string(),
                    )));
                }
            }
            futures::stream::iter(events)
        }
    });

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

// ---------- WebSocket ----------

async fn handle_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Assistant>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// WS 实时模式：每条文本消息触发一轮对话，回复文本逐段实时下发，
/// 结束时发送一个 JSON（完整 ChatOutput，含 session_id / memory / entities / relations）。
async fn handle_socket(mut socket: axum::extract::ws::WebSocket, state: Arc<Assistant>) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            axum::extract::ws::Message::Text(text) => {
                let input = ChatInput {
                    message: text.to_string(),
                    session_id: None,
                    history: vec![],
                    history_count: None,
                };
                let (tx, mut rx) = mpsc::unbounded::<SseMsg>();
                let state = state.clone();

                tokio::task::spawn_blocking(move || {
                    let outcome = state
                        .chat_stream(&input, |delta| {
                            let _ = tx.unbounded_send(SseMsg::Delta(delta.to_string()));
                        })
                        .map_err(|e| e.to_string());
                    let _ = tx.unbounded_send(SseMsg::Done(Box::new(outcome)));
                });

                while let Some(msg) = rx.next().await {
                    let payload = match msg {
                        SseMsg::Delta(text) => text,
SseMsg::Done(outcome) => match *outcome {
                            Ok(out) => serde_json::to_string(&out).unwrap_or_default(),
                            Err(message) => format!("error: {message}"),
                        },
                    };
                    if socket
                        .send(axum::extract::ws::Message::Text(payload.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
            _ => break,
        }
    }
}