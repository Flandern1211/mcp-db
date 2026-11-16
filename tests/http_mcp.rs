use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use mcp_db::transport::http::app;
use mcp_db::{AppState, Driver, Environment, NewGrant, NewSource, Store, Tool};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

fn setup() -> (TempDir, AppState, String) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let token = store.create_token("agent-a").expect("issue token");
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        sessions: Default::default(),
    };
    (dir, state, token.token)
}

fn request(
    method: Method,
    uri: &str,
    body: &str,
    token: Option<&str>,
    session: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(session) = session {
        builder = builder
            .header("mcp-session-id", session)
            .header("mcp-protocol-version", "2025-06-18");
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn send(router: axum::Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read response")
        .to_bytes();
    (status, headers, parse_body(&bytes))
}

fn parse_body(bytes: &[u8]) -> Value {
    let text = String::from_utf8_lossy(bytes);
    if text.trim().is_empty() {
        return Value::Null;
    }
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str(data.trim()) {
                return value;
            }
        }
    }
    serde_json::from_str(text.trim()).expect("JSON or SSE body")
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
const LIST_SOURCES: &str = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}"#;

#[tokio::test]
async fn mcp_requires_a_bearer_token() {
    let (_dir, state, _token) = setup();
    let (status, _, _) = send(
        app(state),
        request(Method::POST, "/mcp", INITIALIZE, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_lists_only_granted_sources_and_binds_sessions_to_tokens() {
    let (_dir, state, token) = setup();
    let router = app(state.clone());

    let (status, headers, _) = send(
        router.clone(),
        request(Method::POST, "/mcp", INITIALIZE, Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = headers
        .get("mcp-session-id")
        .expect("session header")
        .to_str()
        .expect("session value")
        .to_string();

    let (status, _, _) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            INITIALIZED,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            TOOLS_LIST,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tools = body["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "list_sources");

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["result"]["structuredContent"]["sources"]
        .as_array()
        .expect("sources array")
        .is_empty());

    {
        let store = state.store.lock().expect("store lock");
        let source = store
            .create_source(&NewSource {
                id: "a".to_string(),
                driver: Driver::Postgres,
                environment: Environment::NonProduction,
                parent_source_id: None,
                created_token_id: None,
                created_agent: None,
                created_origin: None,
            })
            .expect("create source");
        let identity = store.authenticate(&token).expect("auth");
        store
            .create_grant(&NewGrant {
                token_id: identity.token_id,
                source_id: source.id,
                tool: Tool::QuerySql,
                policy: None,
            })
            .expect("grant");
    }

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sources = body["result"]["structuredContent"]["sources"]
        .as_array()
        .expect("sources array");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["id"], "a");

    let other = {
        let store = state.store.lock().expect("store lock");
        store
            .create_token("agent-b")
            .expect("issue other token")
            .token
    };
    let (status, _, _) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            TOOLS_LIST,
            Some(&other),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
