use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mcp_db::{
    admin_app, AdminState, AppState, ApprovalChannel, ApprovalDraft, ApprovalStatus, Driver,
    Environment, NewApprovalRequest, NewGrant, NewProfile, NewRoute, NewSource, PeerElicitation,
    ProfilePurpose, ProvisionLimits, ReservationStatus, Store, TokenRateLimiter, Tool,
    UnavailableExecutor, UnavailableMetadata, UnavailableProvisioner, ADMIN_LOGIN_TTL_MS,
    DEFAULT_MAX_ROWS,
};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    _dir: TempDir,
    store: Arc<Mutex<Store>>,
    state: AppState,
    admin: Router,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn setup() -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let store = Arc::new(Mutex::new(store));
    let state = AppState {
        store: store.clone(),
        metadata: Arc::new(UnavailableMetadata),
        executor: Arc::new(UnavailableExecutor),
        provisioner: Arc::new(UnavailableProvisioner("test".to_string())),
        provision_limits: ProvisionLimits::default(),
        elicitation: Arc::new(PeerElicitation),
        rate_limiter: Arc::new(TokenRateLimiter::default()),
        max_rows: DEFAULT_MAX_ROWS,
        approval_timeout_ms: 120_000,
        approval_execution_ttl_ms: 60_000,
        sessions: Default::default(),
    };
    let admin = admin_app(AdminState::new(
        store.clone(),
        ProvisionLimits::default(),
        60_000,
    ));
    Fixture {
        _dir: dir,
        store,
        state,
        admin,
    }
}

fn login_token(fixture: &Fixture, now: i64) -> String {
    let store = fixture.store.lock().expect("store lock");
    store
        .create_admin_login_token(now, ADMIN_LOGIN_TTL_MS)
        .expect("login token")
}

fn admin_request(
    method: Method,
    uri: &str,
    body: &str,
    cookies: Option<&str>,
    origin: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:8788")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookies) = cookies {
        builder = builder.header(header::COOKIE, cookies);
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn send_text(router: Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read response")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn set_cookie_values(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().expect("cookie").to_string())
        .collect()
}

/// Logs in and returns `(cookie header for requests, csrf form value)`.
async fn login(fixture: &Fixture) -> (String, String) {
    let token = login_token(fixture, now_ms());
    let (status, headers, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/login",
            &format!("token={token}"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login should redirect");
    let cookies = set_cookie_values(&headers);
    assert_eq!(cookies.len(), 2, "session and csrf cookies: {cookies:?}");
    assert!(cookies.iter().any(|cookie| cookie.contains("HttpOnly")));
    let mut header = String::new();
    let mut csrf = String::new();
    for cookie in &cookies {
        let pair = cookie.split(';').next().expect("cookie pair").to_string();
        let (name, value) = pair.split_once('=').expect("cookie value");
        if name == "mcpdb_admin" {
            header.push_str(&pair);
        } else if name == "mcpdb_csrf" {
            csrf = value.to_string();
            if !header.is_empty() {
                header.push_str("; ");
            }
            header.push_str(&pair);
        }
    }
    assert!(!csrf.is_empty(), "csrf cookie");
    (header, csrf)
}

#[tokio::test]
async fn login_token_is_single_use_and_expires() {
    let fixture = setup();
    let token = login_token(&fixture, now_ms());

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/login",
            &format!("token={token}"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/login",
            &format!("token={token}"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "token is single use");

    let expired = login_token(&fixture, now_ms() - ADMIN_LOGIN_TTL_MS * 2);
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/login",
            &format!("token={expired}"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "token must expire");
}

#[tokio::test]
async fn csrf_is_required_and_checked() {
    let fixture = setup();

    // Without a session, browser navigation redirects but writes are unauthorized.
    let (status, headers, _) = send_text(
        fixture.admin.clone(),
        admin_request(Method::GET, "/admin", "", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin/login")
    );
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/approvals/1/approve",
            "csrf=x",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (cookie, csrf) = login(&fixture).await;

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/approvals/1/approve",
            "",
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "missing csrf");

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/approvals/1/approve",
            "csrf=wrong",
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "wrong csrf");

    // A valid csrf passes the guard and reaches the store (approval does not exist).
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/approvals/1/approve",
            &format!("csrf={csrf}"),
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn wrong_origin_is_rejected_before_consuming_the_token() {
    let fixture = setup();
    let token = login_token(&fixture, now_ms());

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/login",
            &format!("token={token}"),
            None,
            Some("http://evil.example"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The rejected request must not have burned the token.
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/login",
            &format!("token={token}"),
            None,
            Some("http://127.0.0.1:8788"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn agent_bearer_token_cannot_open_admin() {
    let fixture = setup();
    let agent = {
        let store = fixture.store.lock().expect("store lock");
        store.create_token("agent-a").expect("agent token").token
    };
    let (cookie, _) = login(&fixture).await;

    for cookie in [None, Some(cookie.as_str())] {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri("/admin")
            .header(header::HOST, "127.0.0.1:8788")
            .header(header::AUTHORIZATION, format!("Bearer {agent}"));
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let (status, _, _) = send_text(
            fixture.admin.clone(),
            builder.body(Body::empty()).expect("request"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "agent token must never authenticate the admin UI"
        );
    }
}

fn create_pending_approval(fixture: &Fixture) -> i64 {
    let store = fixture.store.lock().expect("store lock");
    let token = store.create_token("agent-a").expect("token");
    store
        .create_source(&NewSource {
            id: "a".to_string(),
            driver: Driver::Postgres,
            environment: Environment::NonProduction,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: None,
        })
        .expect("source");
    let profile = store
        .create_profile(&NewProfile {
            source_id: "a".to_string(),
            name: "mutate".to_string(),
            purpose: ProfilePurpose::Mutate,
            credential_ref: "keyring:mcp-db/test/mutate".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(1_000),
        })
        .expect("profile");
    let route = store
        .create_route(&NewRoute {
            source_id: "a".to_string(),
            tool: Tool::MutateSql,
            profile_id: profile.id,
            min_approval_policy: mcp_db::ApprovalPolicy::Direct,
        })
        .expect("route");
    store
        .create_grant(&NewGrant {
            token_id: token.token_id.clone(),
            source_id: "a".to_string(),
            tool: Tool::MutateSql,
            policy: Some(mcp_db::ApprovalPolicy::Approval),
        })
        .expect("grant");
    let draft = ApprovalDraft {
        token_id: token.token_id.clone(),
        agent: token.agent.clone(),
        source_id: "a".to_string(),
        source_environment: Environment::NonProduction,
        tool: Tool::MutateSql,
        sql: Some("UPDATE users SET name = 'x' WHERE id = 1".to_string()),
        params_json: "[]".to_string(),
        profile_id: profile.id,
        profile_version: profile.version,
        route_id: route.id,
        route_version: route.version,
        grant_policy: mcp_db::ApprovalPolicy::Approval,
        route_min_policy: mcp_db::ApprovalPolicy::Direct,
        high_risk: false,
    };
    let now = now_ms();
    store
        .insert_approval_request(&NewApprovalRequest {
            draft,
            channel: ApprovalChannel::Admin,
            status: ApprovalStatus::Pending,
            created_at: now,
            expires_at: now + 120_000,
            decided_at: None,
            decided_by: None,
            reason_code: None,
            reason: None,
        })
        .expect("pending approval")
        .id
}

#[tokio::test]
async fn approval_decision_runs_once() {
    let fixture = setup();
    let id = create_pending_approval(&fixture);
    let (cookie, csrf) = login(&fixture).await;
    let body = format!("csrf={csrf}");

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            &format!("/admin/approvals/{id}/approve"),
            &body,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let decided_at = {
        let store = fixture.store.lock().expect("store lock");
        let record = store.get_approval(id).expect("approval");
        assert_eq!(record.status, ApprovalStatus::Approved);
        assert_eq!(record.decided_by.as_deref(), Some("admin-web"));
        record.decided_at
    };

    // Replaying the same URL must not apply the decision again.
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            &format!("/admin/approvals/{id}/approve"),
            &body,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let store = fixture.store.lock().expect("store lock");
    let record = store.get_approval(id).expect("approval");
    assert_eq!(record.status, ApprovalStatus::Approved);
    assert_eq!(record.decided_at, decided_at);
    let approvals = store
        .list_recent_audit(100)
        .expect("audit")
        .into_iter()
        .filter(|event| event.approval_id == Some(id) && event.event == "approved")
        .count();
    assert_eq!(approvals, 1, "decision audited exactly once");
}

fn grant_read_access(fixture: &Fixture) -> String {
    let store = fixture.store.lock().expect("store lock");
    let token = store.create_token("agent-a").expect("token");
    store
        .create_source(&NewSource {
            id: "a".to_string(),
            driver: Driver::Postgres,
            environment: Environment::NonProduction,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: None,
        })
        .expect("source");
    let profile = store
        .create_profile(&NewProfile {
            source_id: "a".to_string(),
            name: "query".to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(1_000),
        })
        .expect("profile");
    store
        .create_route(&NewRoute {
            source_id: "a".to_string(),
            tool: Tool::ListTables,
            profile_id: profile.id,
            min_approval_policy: mcp_db::ApprovalPolicy::Direct,
        })
        .expect("route");
    store
        .create_grant(&NewGrant {
            token_id: token.token_id.clone(),
            source_id: "a".to_string(),
            tool: Tool::ListTables,
            policy: None,
        })
        .expect("grant");
    token.token
}

fn mcp_request(body: &str, token: &str, session: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(session) = session {
        builder = builder
            .header("mcp-session-id", session)
            .header("mcp-protocol-version", "2025-06-18");
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn send_json(router: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, text) = send_text(router, request).await;
    (status, headers, parse_body(&text))
}

fn parse_body(text: &str) -> Value {
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
    serde_json::from_str(text.trim()).unwrap_or(Value::Null)
}

async fn init_mcp_session(router: Router, token: &str) -> String {
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let (status, headers, _) =
        send_json(router.clone(), mcp_request(initialize, token, None)).await;
    assert_eq!(status, StatusCode::OK);
    let session = headers
        .get("mcp-session-id")
        .expect("session header")
        .to_str()
        .expect("session value")
        .to_string();
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let (status, _, _) = send_json(router, mcp_request(initialized, token, Some(&session))).await;
    assert!(status.is_success());
    session
}

#[tokio::test]
async fn archived_source_disappears_from_mcp_list_sources() {
    let fixture = setup();
    let token = grant_read_access(&fixture);
    let mcp = mcp_db::transport::http::app(fixture.state.clone());
    let session = init_mcp_session(mcp.clone(), &token).await;
    let list_sources = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}"#;

    let (status, _, body) = send_json(
        mcp.clone(),
        mcp_request(list_sources, &token, Some(&session)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sources = body["result"]["structuredContent"]["sources"]
        .as_array()
        .expect("sources");
    assert_eq!(sources.len(), 1);

    let (cookie, csrf) = login(&fixture).await;
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/sources/a/archive",
            &format!("csrf={csrf}"),
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (status, _, body) = send_json(mcp, mcp_request(list_sources, &token, Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    let sources = body["result"]["structuredContent"]["sources"]
        .as_array()
        .expect("sources");
    assert!(
        sources.is_empty(),
        "archived source must disappear: {sources:?}"
    );
}

#[tokio::test]
async fn web_settles_quarantined_reservation() {
    let fixture = setup();
    let reservation_id = {
        let store = fixture.store.lock().expect("store lock");
        let token = store.create_token("agent-a").expect("token");
        store
            .create_source(&NewSource {
                id: "a".to_string(),
                driver: Driver::Postgres,
                environment: Environment::NonProduction,
                parent_source_id: None,
                created_token_id: None,
                created_agent: None,
                created_origin: None,
            })
            .expect("source");
        let limits = ProvisionLimits::default();
        let now = now_ms();
        let reservation = store
            .reserve_provision(
                &token.token_id,
                "a",
                "mcpdb_np_abcdefghij",
                None,
                &limits,
                now,
            )
            .expect("reserve");
        store
            .settle_reservation(
                reservation.id,
                ReservationStatus::Quarantined,
                Some("EXECUTION_UNKNOWN"),
                now + 1,
            )
            .expect("quarantine");
        reservation.id
    };

    let (cookie, csrf) = login(&fixture).await;
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            &format!("/admin/reservations/{reservation_id}/release"),
            &format!("csrf={csrf}"),
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let store = fixture.store.lock().expect("store lock");
    let reservation = store.get_reservation(reservation_id).expect("reservation");
    assert_eq!(reservation.status, ReservationStatus::Released);
    assert_eq!(reservation.reason_code.as_deref(), Some("ADMIN_SETTLED"));
}

#[tokio::test]
async fn dashboard_shows_audit_without_credentials() {
    let fixture = setup();
    let id = create_pending_approval(&fixture);
    let (cookie, csrf) = login(&fixture).await;
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            &format!("/admin/approvals/{id}/approve"),
            &format!("csrf={csrf}"),
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (status, headers, page) = send_text(
        fixture.admin.clone(),
        admin_request(Method::GET, "/admin", "", Some(&cookie), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(page.contains("Recent audit"));
    assert!(page.contains("approved"));
    assert!(page.contains("admin-web"));
    assert!(page.contains("Sources"));
    assert!(page.contains("a"));
    assert!(
        page.contains(&format!("value=\"{csrf}\"")),
        "csrf form field"
    );
    for secret in ["keyring:", "password", "mcpdb_admin_", "mcpdb_"] {
        assert!(
            !page.contains(secret),
            "admin page must not expose {secret}"
        );
    }
}
