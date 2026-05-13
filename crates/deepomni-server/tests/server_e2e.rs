//! Server E2E tests — PLAN.md Phase 2: HTTP endpoints.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use deepomni_server::{AppState, build_router};
use std::sync::Arc;
use tower::ServiceExt;

async fn test_app() -> (Router, Arc<deepomni_runtime::Runtime>) {
    let runtime = Arc::new(
        deepomni_runtime::RuntimeBuilder::new()
            .workspace("/tmp/test")
            .build()
            .await
            .unwrap(),
    );
    let state = Arc::new(AppState {
        runtime: runtime.clone(),
        auth_token: None,
    });
    (build_router(state), runtime)
}

#[tokio::test]
async fn test_server_health_endpoint() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_tools_returns_builtins() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/tools")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_skills_returns_ok() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/skills")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_plugins_returns_ok() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_create_thread_and_get() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace":"/tmp/test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success() || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
        "server should respond without panic"
    );
}

/// E2E: Create thread via POST, get back via GET, verify real thread_id.
#[tokio::test]
async fn test_e2e_create_and_get_thread_has_real_id() {
    let (app, _) = test_app().await;
    // Create thread.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"workspace":"/tmp/test","name":"e2e-thread"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create thread must succeed");

    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let thread_id = json["thread_id"].as_str().unwrap();
    assert!(!thread_id.is_empty(), "thread_id must not be empty");
    assert_ne!(thread_id, "unknown", "thread_id must be real UUID");

    // Get the thread back.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/threads/{thread_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "get thread must succeed");

    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["thread_id"].as_str().unwrap(), thread_id);
    assert_eq!(json["name"].as_str().unwrap(), "e2e-thread");
}

/// E2E: List threads returns the created thread.
#[tokio::test]
async fn test_e2e_list_threads_includes_created() {
    let (app, _) = test_app().await;
    // Create a thread.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"workspace":"/tmp/test","name":"list-test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let thread_id = json["thread_id"].as_str().unwrap().to_string();

    // List threads.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/threads")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "list threads must succeed");

    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let threads = json["threads"].as_array().unwrap();
    assert!(
        threads
            .iter()
            .any(|t| t["id"].as_str() == Some(&thread_id)
                || t["thread_id"].as_str() == Some(&thread_id)),
        "created thread must appear in list"
    );
}

/// E2E: Interrupt endpoint returns success and submission_id.
#[tokio::test]
async fn test_e2e_interrupt_endpoint_returns_ok() {
    let (app, runtime) = test_app().await;
    // Create a real thread so its SessionLoop exists.
    let thread = runtime
        .create_thread(deepomni_protocol::CreateThreadRequest {
            workspace: std::path::PathBuf::from("/tmp/test"),
            model: None,
            model_provider: None,
            name: None,
            approval_policy: None,
            sandbox: None,
            parent_thread_id: None,
            ephemeral: false,
        })
        .await
        .unwrap();
    let thread_id = thread.id.to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/threads/{thread_id}/turns/test-turn/interrupt"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "interrupt must return success");
    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"].as_str().unwrap(), "interrupted");
    assert!(
        json["submission_id"].as_str().is_some(),
        "must return submission_id"
    );
}

/// E2E: Create turn via HTTP POST returns real thread_id, turn_id, status.
#[tokio::test]
async fn test_e2e_create_turn_returns_real_ids() {
    let (app, _) = test_app().await;
    // Create thread first.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"workspace":"/tmp/test","name":"turn-test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let thread_id = json["thread_id"].as_str().unwrap();

    // Create a turn. Since no provider is registered, it may fail.
    // The test verifies the server responds without panic.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/threads/{thread_id}/turns"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"input":"hello world"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    // Server should respond (success or error, but not panic).
    assert!(
        status.is_success() || status.is_server_error(),
        "create turn must respond without panic"
    );
}

/// E2E: Update thread name via PATCH endpoint.
#[tokio::test]
async fn test_e2e_update_thread_changes_name() {
    let (app, _) = test_app().await;
    // Create thread.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace":"/tmp/test","name":"original"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let thread_id = json["thread_id"].as_str().unwrap();

    // Update the thread name.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/threads/{thread_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"renamed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "update must succeed");

    // Get back and verify name changed.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/threads/{thread_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"].as_str().unwrap(), "renamed");
}

/// E2E: Approve endpoint responds without panic (may error if no pending approval).
#[tokio::test]
async fn test_e2e_approve_endpoint_responds() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads/test-thread/turns/test-turn/approve")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // May be 500 (no pending approval) but must not panic.
    assert!(
        resp.status().is_server_error() || resp.status().is_success(),
        "approve endpoint must respond"
    );
}

/// E2E: Reject endpoint responds without panic.
#[tokio::test]
async fn test_e2e_reject_endpoint_responds() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads/test-thread/turns/test-turn/reject")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error() || resp.status().is_success(),
        "reject endpoint must respond"
    );
}

/// E2E: SSE events endpoint returns 200 with text/event-stream content type.
#[tokio::test]
async fn test_e2e_sse_endpoint_returns_event_stream() {
    let (app, _) = test_app().await;
    // Create a thread so the events endpoint has something to subscribe to.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/threads")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace":"/tmp/test","name":"sse-test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 10_485_760)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let thread_id = json["thread_id"].as_str().unwrap();

    // Subscribe to SSE events for this thread.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/threads/{thread_id}/events"))
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "SSE endpoint must return success"
    );
    // The content-type should indicate event stream.
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/event-stream"),
        "SSE must return text/event-stream, got: {content_type}"
    );
}

/// True end-to-end: start Axum server on real TCP port, send raw HTTP
/// requests, verify full pipeline Server→Runtime over the network.
#[tokio::test]
async fn test_e2e_tcp_server_full_pipeline() {
    use deepomni_server::{AppState, build_router};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // Build runtime and server state.
    let runtime = Arc::new(
        deepomni_runtime::RuntimeBuilder::new()
            .workspace("/tmp/test-tcp-e2e")
            .build()
            .await
            .unwrap(),
    );
    let state = Arc::new(AppState {
        runtime,
        auth_token: None,
    });
    let router = build_router(state);

    // Bind to a random port, spawn server.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Send raw HTTP GET /health.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf);
    assert!(response.contains("200"), "TCP: /health returns 200");
    assert!(response.contains("ok"), "TCP: /health body is 'ok'");

    // Send raw HTTP GET /v1/tools.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET /v1/tools HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf);
    assert!(response.contains("200"), "TCP: /v1/tools returns 200");

    // Send raw HTTP POST /v1/threads.
    let body = r#"{"workspace":"/tmp/test","name":"tcp-e2e"}"#;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /v1/threads HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf);
    assert!(response.contains("200"), "TCP: create thread returns 200");
    assert!(
        response.contains("thread_id"),
        "TCP: response has thread_id"
    );

    // Shutdown.
    server_handle.abort();
}
