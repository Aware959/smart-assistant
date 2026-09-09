use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::Assistant;

#[derive(Debug, Deserialize)]
pub struct AddMemoryRequest {
    pub content: String,
    /// 事实类型：fact / preference / personal / todo / event 等。
    #[serde(default = "default_memory_type")]
    pub memory_type: String,
    /// 来源消息 id（对话沉淀时才有，可选）。
    pub message_id: Option<String>,
}

fn default_memory_type() -> String {
    "fact".to_string()
}

pub fn routes(state: Arc<Assistant>) -> Router {
    Router::new()
        .route("/memory", post(add_memory))
        .route("/memory/{id}", delete(delete_memory))
        .route("/memories", get(list_memories))
        .route("/search", post(search_memory))
        .route("/entities", get(list_entities))
        .route("/relations", get(list_relations))
        .route("/relations/{id}", delete(delete_relation))
        .route("/entities/{id}", delete(delete_entity))
        .route("/sessions", get(list_sessions))
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", delete(delete_session))
        .route("/sessions/{id}/messages", get(list_messages))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub title: String,
}

async fn create_session(
    State(state): State<Arc<Assistant>>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<crate::SessionRecord>), (StatusCode, String)> {
    let id = state
        .create_session(&req.title)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let session = state
        .list_sessions(1)
        .ok()
        .and_then(|s| s.into_iter().find(|s| s.id == id))
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    Ok((StatusCode::CREATED, Json(session)))
}

async fn list_sessions(
    State(state): State<Arc<Assistant>>,
) -> Result<Json<Vec<crate::SessionRecord>>, (StatusCode, String)> {
    let sessions = state
        .list_sessions(100)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(sessions))
}

async fn delete_session(
    State(state): State<Arc<Assistant>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .delete_session(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_messages(
    State(state): State<Arc<Assistant>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::MessageRecord>>, (StatusCode, String)> {
    let messages = state
        .list_messages(&id, 500)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(messages))
}

async fn add_memory(
    State(state): State<Arc<Assistant>>,
    Json(req): Json<AddMemoryRequest>,
) -> Result<(StatusCode, Json<crate::MemoryRecord>), (StatusCode, String)> {
    let id = state
        .add_memory(&req.content, &req.memory_type, req.message_id.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let memory = state
        .list_memories(1)
        .ok()
        .and_then(|m| m.into_iter().find(|m| m.id == id))
        .ok_or_else(|| (StatusCode::NOT_FOUND, "memory not found".to_string()))?;
    Ok((StatusCode::CREATED, Json(memory)))
}

async fn delete_memory(
    State(state): State<Arc<Assistant>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .delete_memory(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_memories(
    State(state): State<Arc<Assistant>>,
) -> Result<Json<Vec<crate::MemoryRecord>>, (StatusCode, String)> {
    let memories = state
        .list_memories(100)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(memories))
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    5
}

async fn search_memory(
    State(state): State<Arc<Assistant>>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Vec<crate::MemoryHit>>, (StatusCode, String)> {
    let hits = state
        .search_memory(&req.query, req.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(hits))
}

async fn list_entities(
    State(state): State<Arc<Assistant>>,
) -> Result<Json<Vec<crate::EntityRecord>>, (StatusCode, String)> {
    let entities = state
        .list_entities()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(entities))
}

async fn list_relations(
    State(state): State<Arc<Assistant>>,
) -> Result<Json<Vec<crate::RelationRecord>>, (StatusCode, String)> {
    let relations = state
        .list_relations()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(relations))
}

async fn delete_relation(
    State(state): State<Arc<Assistant>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .delete_relation(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_entity(
    State(state): State<Arc<Assistant>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .delete_entity(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}