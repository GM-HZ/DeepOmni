//! # DeepOmni Server
//!
//! HTTP/SSE server providing the runtime API over REST endpoints.
//! V1 basic shape: Axum server, health + thread + turn + event endpoints.
//!
//! PRD §7.18

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    middleware,
    response::sse::{Event, Sse},
    routing::{get, post},
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

use deepomni_protocol::id::ThreadId;
use deepomni_protocol::op::{Op, TurnSettings, UserInput};

pub mod protocol_adapter;

/// Shared application state.
pub struct AppState {
    pub runtime: Arc<deepomni_runtime::Runtime>,
    pub auth_token: Option<String>,
}

/// Build the full Axum router with all endpoints.
pub fn build_router(state: Arc<AppState>) -> Router {
    let auth_required = state.auth_token.is_some();

    // All /v1/ endpoints require auth when token is configured.
    let v1_routes = Router::new()
        .route("/threads", post(create_thread))
        .route("/threads", get(list_threads))
        .route("/threads/{id}", get(get_thread))
        .route("/threads/{id}", axum::routing::patch(update_thread))
        .route("/threads/{id}/turns", post(create_turn))
        .route("/threads/{id}/turns/{turn_id}/approve", post(approve_tool))
        .route("/threads/{id}/turns/{turn_id}/reject", post(reject_tool))
        .route(
            "/threads/{id}/turns/{turn_id}/interrupt",
            post(interrupt_turn),
        )
        .route("/threads/{id}/events", get(stream_events))
        .route("/tools", get(list_tools))
        .route("/skills", get(list_skills))
        .route("/plugins", get(list_plugins));

    let v1_with_auth = if auth_required {
        v1_routes.layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
    } else {
        v1_routes
    };

    Router::new()
        .route("/health", get(health))
        .nest("/v1", v1_with_auth)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Auth middleware: validates bearer token if configured.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(ref expected_token) = state.auth_token {
        let header = request
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        match header {
            Some(token) if token == expected_token => {}
            _ => return Err(StatusCode::UNAUTHORIZED),
        }
    }
    Ok(next.run(request).await)
}

// ── Handlers ──

async fn health() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateThreadBody {
    workspace: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

async fn create_thread(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateThreadBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let request = deepomni_protocol::CreateThreadRequest {
        workspace: std::path::PathBuf::from(&body.workspace),
        model: body.model,
        model_provider: None,
        name: body.name,
        approval_policy: None,
        sandbox: None,
        parent_thread_id: None,
        ephemeral: false,
    };

    let thread = state
        .runtime
        .create_thread(request)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "thread_id": thread.id.to_string(),
        "status": format!("{:?}", thread.status).to_lowercase(),
        "name": thread.name,
        "created_at": thread.created_at,
    })))
}

async fn list_threads(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let threads = state
        .runtime
        .list_threads()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "threads": threads.iter().map(|t| serde_json::json!({
            "id": t.id,
            "preview": t.preview,
            "status": t.status,
            "name": t.name,
        })).collect::<Vec<_>>(),
    })))
}

async fn get_thread(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let thread = state
        .runtime
        .get_thread(&id)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "thread_id": thread.id,
        "preview": thread.preview,
        "status": thread.status,
        "name": thread.name,
        "cwd": thread.cwd,
        "created_at": thread.created_at,
    })))
}

#[derive(Debug, serde::Deserialize)]
struct UpdateThreadBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update_thread(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateThreadBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .runtime
        .update_thread_meta(&id, body.name, body.status)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "thread_id": id,
        "updated": true
    })))
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateTurnBody {
    input: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_token_budget: Option<u64>,
}

async fn create_turn(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Json(body): Json<CreateTurnBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let tid = ThreadId::from_string(thread_id.clone());
    // Codex-style: submit Op, return SubmissionId immediately.
    // Turn lifecycle events stream via SSE.
    let sub_id = state
        .runtime
        .submit(Op::UserInput {
            thread_id: tid,
            input: vec![UserInput::Text {
                text: body.input.clone(),
            }],
            settings: TurnSettings {
                model: body.model.clone(),
                ..Default::default()
            },
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "thread_id": thread_id,
        "submission_id": sub_id.as_str(),
    })))
}

async fn approve_tool(
    State(state): State<Arc<AppState>>,
    Path((thread_id, turn_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let tid = deepomni_protocol::id::ThreadId::from_string(thread_id.clone());
    let tuid = deepomni_protocol::id::TurnId::from_string(turn_id);

    let result = state
        .runtime
        .approve_tool(tid, tuid)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "status": "approved",
        "turn_status": format!("{:?}", result.status),
    })))
}

async fn reject_tool(
    State(state): State<Arc<AppState>>,
    Path((thread_id, turn_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .runtime
        .reject_tool(
            deepomni_protocol::id::ThreadId::from_string(thread_id),
            deepomni_protocol::id::TurnId::from_string(turn_id),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "status": "rejected"
    })))
}

async fn interrupt_turn(
    State(state): State<Arc<AppState>>,
    Path((thread_id, _turn_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let tid = ThreadId::from_string(thread_id);
    let sub_id = state
        .runtime
        .submit(deepomni_protocol::op::Op::Cancel { thread_id: tid })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "status": "interrupted",
        "submission_id": sub_id.to_string(),
    })))
}

#[derive(Debug, serde::Deserialize, Default)]
struct EventsQuery {
    #[serde(default)]
    since_seq: Option<i64>,
}

async fn stream_events(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let tid = ThreadId::from_string(thread_id.clone());
    let since = query.since_seq.unwrap_or(0);

    let stream = async_stream::stream! {
        // 1. Subscribe FIRST to avoid the replay/subscribe gap.
        let mut subscriber = state.runtime.subscribe(tid).await;

        // 2. Capture high-water mark: the last durable seq at subscription time.
        let high_water = state.runtime.replay_events(&thread_id, 0)
            .ok()
            .and_then(|events| events.last().map(|(seq, _)| *seq))
            .unwrap_or(0);

        // 3. Replay persisted events up to the high-water mark.
        if let Ok(historical) = state.runtime.replay_events(&thread_id, since) {
            for (seq, event) in historical {
                if seq > high_water { break; }
                let json = serde_json::to_string(&event).unwrap_or_default();
                let event_type = deepomni_events::event_tag(&event);
                yield Ok(Event::default()
                    .event(event_type)
                    .data(json)
                    .id(seq.to_string()));
            }
        }

        // 4. Drain live events, discarding any with seq <= high_water
        //    (already delivered via replay).
        loop {
            match subscriber.recv().await {
                Ok(envelope) => {
                    if envelope.seq <= high_water { continue; }
                    let json = serde_json::to_string(&envelope.frame).unwrap_or_default();
                    let event_type = deepomni_events::event_tag(&envelope.frame);
                    yield Ok(Event::default()
                        .event(event_type)
                        .data(json)
                        .id(envelope.seq.to_string()));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream)
}

async fn list_tools(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = state.runtime.tool_registry();
    let specs = registry.list_specs().await;
    let tools: Vec<serde_json::Value> = specs
        .iter()
        .map(|spec| match spec {
            deepomni_protocol::tool::ToolSpec::Function(details) => {
                serde_json::json!({
                    "name": details.name,
                    "description": details.description,
                })
            }
        })
        .collect();
    Ok(Json(serde_json::json!({ "tools": tools })))
}

async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let manager = state.runtime.session_services().skill_manager.clone();
    let guard = manager.read().await;
    let skills = guard.list();
    let names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
    Ok(Json(serde_json::json!({ "skills": names })))
}

async fn list_plugins(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let manager = state.runtime.session_services().plugin_manager.clone();
    let guard = manager.read().await;
    let plugins = guard.list_enabled();
    let names: Vec<String> = plugins.iter().map(|p| p.manifest.id.clone()).collect();
    Ok(Json(serde_json::json!({ "plugins": names })))
}

// ── Server launcher ──

/// Launch the HTTP server on the given address.
pub async fn serve(
    runtime: Arc<deepomni_runtime::Runtime>,
    bind_addr: &str,
    auth_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(AppState {
        runtime,
        auth_token,
    });

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("DeepOmni server listening on {bind_addr}");

    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_thread_body_deserialize() {
        let json = r#"{"workspace": "/repo"}"#;
        let body: CreateThreadBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.workspace, "/repo");
        assert!(body.name.is_none());
    }

    #[test]
    fn test_create_turn_body_deserialize() {
        let json = r#"{"input": "hello world"}"#;
        let body: CreateTurnBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.input, "hello world");
        assert!(body.model.is_none());
    }
}
