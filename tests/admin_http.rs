use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mcp_db::state::{ProfileUpdate, RouteUpdate};
use mcp_db::{
    admin_app, AdminState, AppState, ApprovalChannel, ApprovalDraft, ApprovalPolicy,
    ApprovalStatus, Driver, Environment, NewApprovalRequest, NewGrant, NewProfile, NewRoute,
    NewSource, PeerElicitation, ProfilePurpose, ProvisionLimits, ReservationStatus, Store,
    TokenRateLimiter, Tool, UnavailableExecutor, UnavailableMetadata, UnavailableProvisioner,
    ADMIN_LOGIN_TTL_MS, DEFAULT_MAX_ROWS,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:8788";
const ORIGIN: &str = "http://127.0.0.1:8788";

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
        None,
        None,
        None,
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
    body: Option<&Value>,
    cookies: Option<&str>,
    origin: Option<&str>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(cookies) = cookies {
        builder = builder.header(header::COOKIE, cookies);
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-mcpdb-csrf", csrf);
    }
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    builder.body(Body::from(body)).expect("request")
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

fn parse_body(text: &str) -> Value {
    if text.trim().is_empty() {
        return Value::Null;
    }
    for line in text.split(['\n', '\r']) {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str(data.trim()) {
                return value;
            }
        }
    }
    serde_json::from_str(text.trim()).unwrap_or(Value::Null)
}

async fn send_json(router: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, text) = send_text(router, request).await;
    (status, headers, parse_body(&text))
}

fn set_cookie_values(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().expect("cookie").to_string())
        .collect()
}

fn session_cookie(headers: &HeaderMap) -> String {
    let cookies = set_cookie_values(headers);
    assert_eq!(cookies.len(), 1, "one session cookie: {cookies:?}");
    let cookie = &cookies[0];
    assert!(cookie.contains("mcpdb_admin="), "session cookie: {cookie}");
    assert!(cookie.contains("HttpOnly"), "session cookie: {cookie}");
    assert!(
        cookie.contains("SameSite=Strict"),
        "session cookie: {cookie}"
    );
    assert!(cookie.contains("Path=/admin"), "session cookie: {cookie}");
    cookie.split(';').next().expect("cookie pair").to_string()
}

async fn fetch_challenge(fixture: &Fixture) -> String {
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/login-challenge",
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let challenge = body["challenge"].as_str().expect("challenge").to_string();
    assert!(!challenge.is_empty());
    challenge
}

/// Logs in over the normal same-origin path and returns `(cookie, session csrf)`.
async fn login(fixture: &Fixture) -> (String, String) {
    let token = login_token(fixture, now_ms());
    let challenge = fetch_challenge(fixture).await;
    let (status, headers, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some(ORIGIN),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login: {body}");
    let cookie = session_cookie(&headers);
    let csrf = body["csrf"].as_str().expect("session csrf").to_string();
    assert!(!csrf.is_empty());
    (cookie, csrf)
}

fn write_request(
    method: Method,
    uri: &str,
    body: Option<&Value>,
    cookie: &str,
    csrf: &str,
) -> Request<Body> {
    admin_request(method, uri, body, Some(cookie), Some(ORIGIN), Some(csrf))
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
        sql: Some("UPDATE users SET name = $1 WHERE id = $2".to_string()),
        params_json: r#"["secret-business-value",42]"#.to_string(),
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

fn create_read_access(fixture: &Fixture) -> String {
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

async fn mcp_sources(router: Router, token: &str, session: &str) -> Vec<Value> {
    let list_sources = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}"#;
    let (status, _, text) =
        send_text(router, mcp_request(list_sources, token, Some(session))).await;
    assert_eq!(status, StatusCode::OK);
    let body = parse_body(&text);
    body["result"]["structuredContent"]["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("unexpected list_sources response: {text}"))
        .clone()
}

#[tokio::test]
async fn login_token_is_single_use_and_expires() {
    let fixture = setup();
    let token = login_token(&fixture, now_ms());
    let challenge = fetch_challenge(&fixture).await;

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some("null"),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "valid null-origin login with challenge"
    );

    let challenge = fetch_challenge(&fixture).await;
    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some(ORIGIN),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "token is single use");

    let expired = login_token(&fixture, now_ms() - ADMIN_LOGIN_TTL_MS * 2);
    let challenge = fetch_challenge(&fixture).await;
    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": expired })),
            None,
            Some(ORIGIN),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "token must expire");
}

#[tokio::test]
async fn permanent_admin_key_can_log_in_repeatedly_until_revoked() {
    let fixture = setup();
    let key = {
        let store = fixture.store.lock().expect("store lock");
        store.create_admin_key("test", now_ms()).expect("admin key")
    };

    for _ in 0..2 {
        let challenge = fetch_challenge(&fixture).await;
        let (status, _, body) = send_json(
            fixture.admin.clone(),
            admin_request(
                Method::POST,
                "/admin/api/login",
                Some(&json!({ "token": key })),
                None,
                Some(ORIGIN),
                Some(&challenge),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "permanent key login: {body}");
    }

    let id = {
        let store = fixture.store.lock().expect("store lock");
        store.list_admin_keys().expect("list keys")[0].id
    };
    {
        let store = fixture.store.lock().expect("store lock");
        store.revoke_admin_key(id, now_ms()).expect("revoke");
    }
    let challenge = fetch_challenge(&fixture).await;
    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": key })),
            None,
            Some(ORIGIN),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "revoked key is rejected");
}

#[tokio::test]
async fn null_origin_login_requires_a_page_challenge() {
    let fixture = setup();
    let token = login_token(&fixture, now_ms());

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some("null"),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "null origin needs a challenge"
    );

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some("null"),
            Some("forged-challenge"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "wrong challenge");

    // A rejected null request must not burn the login token.
    let challenge = fetch_challenge(&fixture).await;
    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some("null"),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "challenge proves the page context");
}

#[tokio::test]
async fn cross_origin_and_cross_port_are_rejected() {
    let fixture = setup();
    let token = login_token(&fixture, now_ms());

    for origin in [
        "http://evil.example",
        "http://127.0.0.1:9999",
        "https://127.0.0.1:8788",
        "http://127.0.0.1:8788.evil.example",
    ] {
        let challenge = fetch_challenge(&fixture).await;
        let (status, _, _) = send_json(
            fixture.admin.clone(),
            admin_request(
                Method::POST,
                "/admin/api/login",
                Some(&json!({ "token": token })),
                None,
                Some(origin),
                Some(&challenge),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "origin {origin}");
    }

    let challenge = fetch_challenge(&fixture).await;
    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some(ORIGIN),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "rejected requests did not consume token"
    );
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
            .uri("/admin/api/session")
            .header(header::HOST, HOST)
            .header(header::ORIGIN, ORIGIN)
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

#[tokio::test]
async fn session_cookie_and_security_headers_cover_all_responses() {
    let fixture = setup();

    let (status, _, page) = send_text(
        fixture.admin.clone(),
        admin_request(Method::GET, "/admin/login", None, None, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("mcp-db 管理台"));
    assert!(page.contains("/admin/assets/app.js"));
    assert!(page.contains("mcpdb-login-challenge"));
    assert!(!page.contains("__MCPDB_LOGIN_CHALLENGE__"));
    assert!(page.contains("<script src=\"/admin/assets/app.js\" defer></script>"));
    assert!(!page.contains("<script>"));
    let daisy_link = page
        .find("<link rel=\"stylesheet\" href=\"/admin/assets/daisyui.css\" />")
        .expect("daisyUI bundle must be loaded");
    let app_link = page
        .find("<link rel=\"stylesheet\" href=\"/admin/assets/app.css\" />")
        .expect("app styles must be loaded");
    assert!(
        daisy_link < app_link,
        "daisyUI base CSS must load before app overrides"
    );

    let (status, headers, _) = send_text(
        fixture.admin.clone(),
        admin_request(Method::GET, "/admin", None, None, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::FOUND, "no session redirects to login");

    let mut bodies = std::collections::HashMap::new();
    for (path, expected_type) in [
        ("/admin/assets/daisyui.css", Some("text/css")),
        ("/admin/assets/app.css", Some("text/css")),
        ("/admin/assets/app.js", Some("javascript")),
    ] {
        let (status, headers, body) = send_text(
            fixture.admin.clone(),
            admin_request(Method::GET, path, None, None, None, None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(
            content_type.contains(expected_type.unwrap()),
            "{path}: {content_type}"
        );
        assert_security_headers(&headers);
        bodies.insert(path, body);
    }
    assert_security_headers(&headers);

    let daisy_css = &bodies["/admin/assets/daisyui.css"];
    assert!(
        daisy_css.len() > 20_000,
        "generated daisyUI bundle is too small"
    );
    assert!(
        daisy_css.contains("tailwindcss v4"),
        "generated bundle is missing the Tailwind banner"
    );
    for component in [
        ".btn{",
        ".input{",
        ".select{",
        ".checkbox{",
        ".badge{",
        ".alert{",
        ".menu{",
        ".table{",
    ] {
        assert!(
            daisy_css.contains(component),
            "generated bundle is missing {component}"
        );
    }

    let app_css = &bodies["/admin/assets/app.css"];
    assert!(
        app_css.contains("--line:"),
        "app border colour token must not reuse daisyUI's --border width"
    );
    assert!(
        !app_css.contains("--border: #"),
        "app CSS must not shadow daisyUI's --border width with a colour"
    );
    for base_component in ["\n.btn {\n", "\n.input {\n", "\n.select {\n"] {
        assert!(
            !app_css.contains(base_component),
            "app CSS must rely on daisyUI for {base_component}"
        );
    }

    let (status, headers, _) = send_text(
        fixture.admin.clone(),
        admin_request(Method::GET, "/admin/does-not-exist", None, None, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_security_headers(&headers);

    let (_, headers, _) = send_text(
        fixture.admin.clone(),
        admin_request(Method::GET, "/admin/api/session", None, None, None, None),
    )
    .await;
    assert_security_headers(&headers);
}

fn assert_security_headers(headers: &HeaderMap) {
    let header_value = |name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    };
    let csp = header_value(header::CONTENT_SECURITY_POLICY);
    for directive in [
        "default-src 'none'",
        "script-src 'self'",
        "style-src 'self'",
        "img-src 'self' data:",
        "connect-src 'self'",
        "form-action 'self'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directive), "CSP missing {directive}: {csp}");
    }
    assert_eq!(header_value(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert_eq!(header_value(header::REFERRER_POLICY), "no-referrer");
    assert!(header_value(header::CACHE_CONTROL).contains("no-store"));
}

#[tokio::test]
async fn csrf_is_required_and_checked_for_writes() {
    let fixture = setup();

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "agent-a" })),
            None,
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "session required");

    let (cookie, csrf) = login(&fixture).await;

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "agent-a" })),
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "missing csrf");

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "agent-a" })),
            Some(&cookie),
            Some(ORIGIN),
            Some("wrong"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "wrong csrf");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "agent-a" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["plaintext"]
        .as_str()
        .unwrap_or_default()
        .starts_with("mcpdb_"));

    // Reads need the session but no CSRF header.
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/tokens",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tokens"].as_array().expect("tokens").len(), 1);
}

#[tokio::test]
async fn null_origin_writes_still_require_the_session_csrf() {
    let fixture = setup();
    let token = login_token(&fixture, now_ms());
    let challenge = fetch_challenge(&fixture).await;
    let (status, headers, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/login",
            Some(&json!({ "token": token })),
            None,
            Some("null"),
            Some(&challenge),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = session_cookie(&headers);
    let csrf = body["csrf"].as_str().expect("csrf").to_string();

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "agent-null" })),
            Some(&cookie),
            Some("null"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "null alone is not trust");

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "agent-null" })),
            Some(&cookie),
            Some("null"),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "session csrf authorizes the write"
    );
}

#[tokio::test]
async fn source_profile_route_and_environment_apis() {
    let fixture = setup();
    let (cookie, csrf) = login(&fixture).await;

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources",
            Some(&json!({ "id": "a", "environment": "nonproduction" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/profiles",
            Some(&json!({
                "name": "query_ro",
                "purpose": "query",
                "credential_ref": "keyring:mcp-db/a/query",
                "max_connections": 3,
                "statement_timeout_ms": 5000
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let profile_id = body["profile"]["id"].as_i64().expect("profile id");
    assert_eq!(body["profile"]["credential_ref"], "keyring:mcp-db/a/query");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/routes",
            Some(&json!({
                "tool": "list_tables",
                "profile_id": profile_id,
                "min_approval_policy": "direct"
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["route"]["profile_name"], "query_ro");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/sources/a",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["profiles"].as_array().expect("profiles").len(), 1);
    assert_eq!(body["routes"].as_array().expect("routes").len(), 1);
    assert_eq!(body["source"]["profile_count"], 1);
    assert!(!body.to_string().contains("postgres://"));

    // A source must be archived before its environment can be reclassified.
    let (status, _, _) = send_text(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/environment",
            Some(&json!({ "environment": "production" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/archive",
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/environment",
            Some(&json!({ "environment": "production" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"]["environment"], "production");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/restore",
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "restore needs a grant");
    assert!(body["error"].as_str().unwrap_or_default().contains("授权"));
}

#[tokio::test]
async fn production_write_grants_and_wrong_purpose_routes_are_rejected() {
    let fixture = setup();
    let (cookie, csrf) = login(&fixture).await;

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources",
            Some(&json!({ "id": "prod", "environment": "production" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "writer" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token_id = body["token"]["id"].as_str().expect("token id").to_string();

    for tool in ["mutate_sql", "execute_ddl"] {
        let (status, _, body) = send_json(
            fixture.admin.clone(),
            write_request(
                Method::POST,
                &format!("/admin/api/tokens/{token_id}/grants"),
                Some(&json!({ "source_id": "prod", "tool": tool, "policy": "approval" })),
                &cookie,
                &csrf,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{tool}: {body}");
    }

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            &format!("/admin/api/tokens/{token_id}/grants"),
            Some(&json!({ "source_id": "prod", "tool": "query_sql", "policy": "direct" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/prod/profiles",
            Some(&json!({
                "name": "mutate",
                "purpose": "mutate",
                "credential_ref": "keyring:mcp-db/prod/mutate"
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let profile_id = body["profile"]["id"].as_i64().expect("profile id");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/prod/routes",
            Some(&json!({
                "tool": "list_tables",
                "profile_id": profile_id,
                "min_approval_policy": "direct"
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "wrong purpose route: {body}"
    );
}

#[tokio::test]
async fn archived_source_disappears_from_mcp_list_sources() {
    let fixture = setup();
    let token = create_read_access(&fixture);
    let mcp = mcp_db::transport::http::app(fixture.state.clone());
    let session = init_mcp_session(mcp.clone(), &token).await;
    assert_eq!(mcp_sources(mcp.clone(), &token, &session).await.len(), 1);

    let (cookie, csrf) = login(&fixture).await;
    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/a/archive",
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let sources = mcp_sources(mcp, &token, &session).await;
    assert!(
        sources.is_empty(),
        "archived source must disappear: {sources:?}"
    );
}

#[tokio::test]
async fn revoking_a_grant_takes_effect_for_the_next_mcp_call() {
    let fixture = setup();
    let token = create_read_access(&fixture);
    let mcp = mcp_db::transport::http::app(fixture.state.clone());
    let session = init_mcp_session(mcp.clone(), &token).await;
    assert_eq!(mcp_sources(mcp.clone(), &token, &session).await.len(), 1);

    let token_id = {
        let store = fixture.store.lock().expect("store lock");
        store.list_tokens().expect("tokens")[0].id.clone()
    };
    let (cookie, csrf) = login(&fixture).await;
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::DELETE,
            &format!("/admin/api/tokens/{token_id}/grants?source=a&tool=list_tables"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let sources = mcp_sources(mcp, &token, &session).await;
    assert!(
        sources.is_empty(),
        "revoked grant must take effect: {sources:?}"
    );
}

fn grant_source(store: &Store, id: &str, driver: Driver, environment: Environment) {
    store
        .create_source(&NewSource {
            id: id.to_string(),
            driver,
            environment,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: None,
        })
        .expect("source");
}

fn grant_route(store: &Store, id: &str, tool: Tool, policy: ApprovalPolicy) -> mcp_db::Route {
    let purpose = tool.purpose().expect("source tool");
    let profile = store
        .create_profile(&NewProfile {
            source_id: id.to_string(),
            name: tool.as_str().to_string(),
            purpose,
            credential_ref: if purpose == ProfilePurpose::Provision {
                "keyring:provision/test"
            } else {
                "keyring:test/profile"
            }
            .to_string(),
            max_connections: None,
            statement_timeout_ms: None,
        })
        .expect("profile");
    store
        .create_route(&NewRoute {
            source_id: id.to_string(),
            tool,
            profile_id: profile.id,
            min_approval_policy: policy,
        })
        .expect("route")
}

#[tokio::test]
async fn replace_source_grants_is_scoped_and_takes_effect_in_the_existing_mcp_session() {
    use ApprovalPolicy::{Approval, Direct};
    let fixture = setup();
    let bearer = create_read_access(&fixture);
    let (token_id, other) = {
        let store = fixture.store.lock().unwrap();
        let identity = store.authenticate(&bearer).unwrap();
        let other = store.create_token(&identity.agent).unwrap().token_id;
        grant_source(&store, "b", Driver::Postgres, Environment::NonProduction);
        grant_route(&store, "a", Tool::QuerySql, Direct);
        grant_route(&store, "a", Tool::MutateSql, Direct);
        let ddl = grant_route(&store, "a", Tool::ExecuteDdl, Direct);
        for (token_id, source, tool) in [
            (&identity.token_id, "a", Tool::MutateSql),
            (&identity.token_id, "a", Tool::ExecuteDdl),
            (&identity.token_id, "b", Tool::QuerySql),
            (&other, "a", Tool::ListTables),
        ] {
            store
                .create_grant(&NewGrant {
                    token_id: token_id.clone(),
                    source_id: source.to_string(),
                    tool,
                    policy: Some(Direct),
                })
                .unwrap();
        }
        store
            .update_route(
                "a",
                ddl.id,
                &RouteUpdate {
                    profile_id: ddl.profile_id,
                    min_approval_policy: Approval,
                },
            )
            .unwrap();
        (identity.token_id, other)
    };
    let (cookie, csrf) = login(&fixture).await;
    let path = format!("/admin/api/tokens/{token_id}/sources/a/grants");
    let original = json!([
        { "tool": "mutate_sql", "policy": "direct" },
        { "tool": "list_tables", "policy": "direct" },
        { "tool": "execute_ddl", "policy": "direct" },
    ]);
    let updated = json!([
        { "tool": "query_sql", "policy": "direct" },
        { "tool": "execute_ddl", "policy": "direct" },
        { "tool": "mutate_sql", "policy": "approval" },
    ]);
    let change = json!({ "expected": original, "grants": updated });
    let mcp = mcp_db::transport::http::app(fixture.state.clone());
    let session = init_mcp_session(mcp.clone(), &bearer).await;
    assert_eq!(mcp_sources(mcp.clone(), &bearer, &session).await.len(), 2);

    let (status, _, response) = send_json(
        fixture.admin.clone(),
        write_request(Method::PUT, &path, Some(&change), &cookie, &csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        response["grants"],
        json!([
            { "token_id": token_id, "source_id": "a", "tool": "execute_ddl", "policy": "direct" },
            { "token_id": token_id, "source_id": "a", "tool": "mutate_sql", "policy": "approval" },
            { "token_id": token_id, "source_id": "a", "tool": "query_sql", "policy": "direct" },
        ])
    );
    {
        let store = fixture.store.lock().unwrap();
        assert!(store.get_grant(&token_id, "a", Tool::ListTables).is_err());
        let call = mcp_db::Authorizer::new(&store)
            .authorize(&token_id, Some("a"), Tool::ExecuteDdl)
            .unwrap();
        assert_eq!(call.grant_policy, Direct);
        assert_eq!(call.policy, Approval);
    }
    let clear = json!({ "expected": updated, "grants": [] });
    let (status, _, response) = send_json(
        fixture.admin.clone(),
        write_request(Method::PUT, &path, Some(&clear), &cookie, &csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!({ "grants": [] }));
    let visible = mcp_sources(mcp.clone(), &bearer, &session).await;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["id"], "b");
    let mcp_query = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"query_sql","arguments":{"source":"a","sql":"SELECT 1"}}}"#;
    let (status, _, response) =
        send_json(mcp, mcp_request(mcp_query, &bearer, Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "TOOL_NOT_GRANTED"
    );
    let store = fixture.store.lock().unwrap();
    assert_eq!(store.list_grants_for_token(&token_id).unwrap().len(), 1);
    assert!(store.get_grant(&other, "a", Tool::ListTables).is_ok());
}

#[tokio::test]
async fn source_grant_put_checks_admin_boundary_and_rejects_whole_invalid_sets() {
    use ApprovalPolicy::{Approval, Direct};
    let fixture = setup();
    let cookie_csrf = login(&fixture).await;
    let (cookie, csrf) = (&cookie_csrf.0, &cookie_csrf.1);
    let (token_id, before, audit_count) = {
        let store = fixture.store.lock().unwrap();
        let token_id = store.create_token("agent").unwrap().token_id;
        grant_source(&store, "prod", Driver::Postgres, Environment::Production);
        grant_source(&store, "file", Driver::Sqlite, Environment::NonProduction);
        grant_route(&store, "prod", Tool::QuerySql, Direct);
        grant_route(&store, "prod", Tool::MutateSql, Direct);
        grant_route(&store, "prod", Tool::CreateDatabase, Approval);
        grant_route(&store, "file", Tool::CreateDatabase, Approval);
        for tool in [Tool::ListTables, Tool::DescribeTable] {
            let disabled = grant_route(&store, "prod", tool, Direct);
            store
                .update_profile(
                    "prod",
                    disabled.profile_id,
                    &ProfileUpdate {
                        name: tool.as_str().to_string(),
                        enabled: false,
                        credential_ref: None,
                        max_connections: None,
                        statement_timeout_ms: None,
                    },
                )
                .unwrap();
        }
        let before = store
            .create_grant(&NewGrant {
                token_id: token_id.clone(),
                source_id: "prod".to_string(),
                tool: Tool::ListTables,
                policy: Some(Direct),
            })
            .unwrap();
        (token_id, before, store.count_audit_events_all().unwrap())
    };
    let path = format!("/admin/api/tokens/{token_id}/sources/prod/grants");
    let expected = json!([{ "tool": "list_tables", "policy": "direct" }]);
    let query = json!({ "tool": "query_sql", "policy": "direct" });
    let create = json!({ "tool": "create_database", "policy": "approval" });
    let good = json!({ "expected": expected, "grants": [query, create, expected[0]] });
    let clear = json!({ "expected": expected, "grants": [] });
    for (session, origin, header, expected_status) in [
        (None, ORIGIN, Some(csrf.as_str()), StatusCode::UNAUTHORIZED),
        (Some(cookie.as_str()), ORIGIN, None, StatusCode::FORBIDDEN),
        (
            Some(cookie.as_str()),
            ORIGIN,
            Some("wrong"),
            StatusCode::FORBIDDEN,
        ),
        (
            Some(cookie.as_str()),
            "http://127.0.0.1:8787",
            Some(csrf.as_str()),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, headers, _) = send_json(
            fixture.admin.clone(),
            admin_request(
                Method::PUT,
                &path,
                Some(&clear),
                session,
                Some(origin),
                header,
            ),
        )
        .await;
        assert_eq!(status, expected_status);
        assert_security_headers(&headers);
    }
    for (request, expected_status) in [
        (json!({ "grants": [] }), StatusCode::BAD_REQUEST),
        (json!({ "expected": expected }), StatusCode::BAD_REQUEST),
        (
            json!({ "expected": [], "grants": [query] }),
            StatusCode::CONFLICT,
        ),
        (
            json!({ "expected": [expected[0], expected[0]], "grants": [query] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, query] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, { "tool": "create_database" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, { "tool": "create_database", "policy": "direct" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, { "tool": "create_database", "policy": "never" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, { "tool": "mutate_sql", "policy": "approval" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, { "tool": "execute_ddl", "policy": "direct" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "expected": expected, "grants": [query, { "tool": "describe_table", "policy": "direct" }] }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _, body) = send_json(
            fixture.admin.clone(),
            write_request(Method::PUT, &path, Some(&request), cookie, csrf),
        )
        .await;
        assert_eq!(status, expected_status, "{body}");
    }
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::PUT,
            &format!("/admin/api/tokens/{token_id}/sources/file/grants"),
            Some(&json!({ "expected": [], "grants": [create] })),
            cookie,
            csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/sources",
            None,
            Some(cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tools = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["id"] == "prod")
        .unwrap()["grant_tools"]
        .as_array()
        .unwrap();
    assert!(tools.contains(&json!({ "tool": "list_tables", "reason": "连接配置已停用" })));
    assert!(tools.contains(&json!({ "tool": "create_database", "min_policy": "approval" })));
    {
        let store = fixture.store.lock().unwrap();
        assert_eq!(
            store.list_grants_for_token(&token_id).unwrap(),
            vec![before]
        );
        assert_eq!(store.count_audit_events_all().unwrap(), audit_count);
    }
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(Method::PUT, &path, Some(&good), cookie, csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["grants"].as_array().unwrap().len(), 3);

    fixture
        .store
        .lock()
        .unwrap()
        .archive_source("prod", "test", now_ms())
        .unwrap();
    let archived_snapshot = json!([query, create, expected[0]]);
    let invalid = json!({ "expected": archived_snapshot, "grants": [query, { "tool": "explain_query", "policy": "direct" }] });
    let clear = json!({ "expected": archived_snapshot, "grants": [] });
    for (body, expected_status) in [(invalid, StatusCode::BAD_REQUEST), (clear, StatusCode::OK)] {
        let (status, _, response) = send_json(
            fixture.admin.clone(),
            write_request(Method::PUT, &path, Some(&body), cookie, csrf),
        )
        .await;
        assert_eq!(status, expected_status, "{response}");
    }
}

#[tokio::test]
async fn approval_detail_and_duplicate_decision() {
    let fixture = setup();
    let id = create_pending_approval(&fixture);
    let (cookie, csrf) = login(&fixture).await;

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/approvals",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let approvals = body["approvals"].as_array().expect("approvals");
    assert_eq!(approvals.len(), 1);
    let record = &approvals[0];
    assert_eq!(record["sql"], "UPDATE users SET name = $1 WHERE id = $2");
    assert_eq!(record["params_json"], r#"["secret-business-value",42]"#);
    assert!(record["profile_version"].as_i64().unwrap_or_default() >= 1);
    assert!(record["route_version"].as_i64().unwrap_or_default() >= 1);
    assert!(record["request_digest"].as_str().unwrap_or_default().len() == 64);

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            &format!("/admin/api/approvals/{id}/approve"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let decided_at = {
        let store = fixture.store.lock().expect("store lock");
        let record = store.get_approval(id).expect("approval");
        assert_eq!(record.status, ApprovalStatus::Approved);
        assert_eq!(record.decided_by.as_deref(), Some("admin-web"));
        record.decided_at
    };

    let (status, _, _) = send_text(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            &format!("/admin/api/approvals/{id}/approve"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "decision is one-shot");

    let store = fixture.store.lock().expect("store lock");
    let record = store.get_approval(id).expect("approval");
    assert_eq!(record.status, ApprovalStatus::Approved);
    assert_eq!(record.decided_at, decided_at);
    let approved_events = store
        .query_audit(None, None, Some("allowed"), 100, 0)
        .expect("audit")
        .0
        .into_iter()
        .filter(|event| event.approval_id == Some(id) && event.event == "approved")
        .count();
    assert_eq!(approved_events, 1, "decision audited exactly once");
}

#[tokio::test]
async fn audit_filters_paginates_and_never_contains_token_plaintext() {
    let fixture = setup();
    let (cookie, csrf) = login(&fixture).await;

    let (_, _, created) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/tokens",
            Some(&json!({ "agent": "audited" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    let plaintext = created["plaintext"]
        .as_str()
        .expect("plaintext")
        .to_string();
    assert!(plaintext.starts_with("mcpdb_"));

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/audit?limit=1&offset=0",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("items").len(), 1);
    assert!(body["total"].as_i64().unwrap_or_default() >= 1);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/audit?decision=allowed&limit=50",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body["items"].as_array().expect("items").is_empty());
    for item in body["items"].as_array().expect("items") {
        assert_eq!(item["decision"], "allowed");
    }
    let rendered = body.to_string();
    assert!(!rendered.contains(&plaintext));
    assert!(!rendered.contains("token_hash"));
    assert!(!rendered.contains("mcpdb-admin"));
}

#[tokio::test]
async fn overview_reports_real_counts() {
    let fixture = setup();
    create_read_access(&fixture);
    let (cookie, _) = login(&fixture).await;
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/overview",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sources"]["total"], 1);
    assert_eq!(body["sources"]["active"], 1);
    assert_eq!(body["sources"]["archived"], 0);
    assert_eq!(body["tokens"]["active"], 1);
    assert_eq!(body["tokens"]["revoked"], 0);
    assert!(body["audit_total"].as_i64().unwrap_or_default() >= 1);
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
    let (status, _, body) = send_json(
        fixture.admin.clone(),
        admin_request(
            Method::GET,
            "/admin/api/reservations?status=quarantined",
            None,
            Some(&cookie),
            Some(ORIGIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["reservations"].as_array().expect("reservations").len(),
        1
    );

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            &format!("/admin/api/reservations/{reservation_id}/release"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let store = fixture.store.lock().expect("store lock");
    let reservation = store.get_reservation(reservation_id).expect("reservation");
    assert_eq!(reservation.status, ReservationStatus::Released);
    assert_eq!(reservation.reason_code.as_deref(), Some("ADMIN_SETTLED"));
}

#[tokio::test]
async fn profiles_and_routes_support_update_delete_and_versioning() {
    let fixture = setup();
    let (cookie, csrf) = login(&fixture).await;

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources",
            Some(&json!({ "id": "crud", "environment": "nonproduction" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/crud/profiles",
            Some(&json!({
                "name": "query_ro",
                "purpose": "query",
                "credential_ref": "keyring:mcp-db/crud/query",
                "max_connections": 2,
                "statement_timeout_ms": 1000
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let profile_id = body["profile"]["id"].as_i64().expect("profile id");
    assert_eq!(body["profile"]["version"], 1);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::PUT,
            &format!("/admin/api/sources/crud/profiles/{profile_id}"),
            Some(&json!({
                "name": "query_ro",
                "enabled": false,
                "max_connections": 3,
                "statement_timeout_ms": 2000
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["profile"]["version"], 2);
    assert_eq!(body["profile"]["enabled"], false);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::POST,
            "/admin/api/sources/crud/routes",
            Some(&json!({
                "tool": "list_tables",
                "profile_id": profile_id
            })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let route_id = body["route"]["id"].as_i64().expect("route id");
    assert_eq!(body["route"]["version"], 1);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::PUT,
            &format!("/admin/api/sources/crud/routes/{route_id}"),
            Some(&json!({ "profile_id": profile_id, "min_approval_policy": "direct" })),
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["route"]["version"], 2);

    let (status, _, body) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::DELETE,
            &format!("/admin/api/sources/crud/profiles/{profile_id}"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "referenced profile: {body}");

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::DELETE,
            &format!("/admin/api/sources/crud/routes/{route_id}"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send_json(
        fixture.admin.clone(),
        write_request(
            Method::DELETE,
            &format!("/admin/api/sources/crud/profiles/{profile_id}"),
            None,
            &cookie,
            &csrf,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
