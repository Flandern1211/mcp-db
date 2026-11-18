//! Local-only web admin: one-time login token, in-process session, CSRF-checked JSON API.
//!
//! The admin listener is separate from the MCP listener and is expected to be bound or
//! published to the host loopback address only. Sessions live in process memory and are
//! lost on restart. Cookies are not marked `Secure` because the first version is served
//! over plain loopback HTTP; cross-host access requires TLS and is out of scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approval::AuditEvent;
use crate::auth;
use crate::provision::ProvisionLimits;
use crate::state::{
    ApprovalPolicy, Environment, NewGrant, NewProfile, NewRoute, NewSource, Profile,
    ProfilePurpose, ReservationStatus, Route, Source, Store, Tool,
};
use crate::DecisionOutcome;
use crate::Error;

/// One-time login token lifetime. The CLI prints the plaintext once.
pub const ADMIN_LOGIN_TTL_MS: i64 = 5 * 60 * 1000;
/// In-process admin session lifetime.
pub const ADMIN_SESSION_TTL_MS: i64 = 60 * 60 * 1000;
pub const SESSION_COOKIE: &str = "mcpdb_admin";
const ADMIN_ACTOR: &str = "admin-web";
const LOGIN_CHALLENGE_TTL_MS: i64 = 5 * 60 * 1000;
const CSRF_HEADER: &str = "x-mcpdb-csrf";
const SECURITY_HEADERS_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

const INDEX_HTML: &str = include_str!("admin_assets/index.html");
const APP_CSS: &str = include_str!("admin_assets/app.css");
const APP_JS: &str = include_str!("admin_assets/app.js");

#[derive(Debug, Clone)]
pub struct AdminSession {
    pub csrf: String,
    pub expires_at: i64,
}

#[derive(Clone)]
pub struct AdminState {
    pub store: Arc<Mutex<Store>>,
    pub provision_limits: ProvisionLimits,
    pub approval_execution_ttl_ms: i64,
    pub sessions: Arc<Mutex<HashMap<String, AdminSession>>>,
    pub login_challenges: Arc<Mutex<HashMap<String, i64>>>,
}

impl AdminState {
    pub fn new(
        store: Arc<Mutex<Store>>,
        provision_limits: ProvisionLimits,
        approval_execution_ttl_ms: i64,
    ) -> Self {
        Self {
            store,
            provision_limits,
            approval_execution_ttl_ms,
            sessions: Default::default(),
            login_challenges: Default::default(),
        }
    }

    fn issue_login_challenge(&self, now: i64) -> String {
        let challenge = auth::generate_secret();
        if let Ok(mut challenges) = self.login_challenges.lock() {
            challenges.retain(|_, expires_at| *expires_at > now);
            challenges.insert(challenge.clone(), now + LOGIN_CHALLENGE_TTL_MS);
        }
        challenge
    }

    fn login_challenge_valid(&self, challenge: &str, now: i64) -> bool {
        self.login_challenges
            .lock()
            .ok()
            .map(|challenges| {
                challenges
                    .get(challenge)
                    .map(|expires_at| *expires_at > now)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn consume_login_challenge(&self, challenge: &str) {
        if let Ok(mut challenges) = self.login_challenges.lock() {
            challenges.remove(challenge);
        }
    }
}

pub fn admin_app(state: AdminState) -> Router {
    Router::new()
        .route("/admin", get(index))
        .route("/admin/login", get(login_page))
        .route("/admin/assets/app.css", get(app_css))
        .route("/admin/assets/app.js", get(app_js))
        .route("/admin/api/login-challenge", get(login_challenge))
        .route("/admin/api/login", post(login))
        .route("/admin/api/logout", post(logout))
        .route("/admin/api/session", get(session_info))
        .route("/admin/api/overview", get(overview))
        .route("/admin/api/sources", get(list_sources).post(create_source))
        .route("/admin/api/sources/{id}", get(source_detail))
        .route("/admin/api/sources/{id}/environment", post(set_environment))
        .route("/admin/api/sources/{id}/archive", post(archive))
        .route("/admin/api/sources/{id}/restore", post(restore))
        .route("/admin/api/sources/{id}/profiles", post(create_profile))
        .route("/admin/api/sources/{id}/routes", post(create_route))
        .route("/admin/api/tokens", get(list_tokens).post(create_token))
        .route("/admin/api/tokens/{id}/revoke", post(revoke_token))
        .route(
            "/admin/api/tokens/{id}/grants",
            get(list_grants).post(add_grant).delete(remove_grant),
        )
        .route("/admin/api/approvals", get(list_approvals))
        .route("/admin/api/approvals/{id}/approve", post(approve))
        .route("/admin/api/approvals/{id}/deny", post(deny))
        .route("/admin/api/audit", get(audit))
        .route("/admin/api/reservations", get(list_reservations))
        .route("/admin/api/reservations/{id}/release", post(release))
        .route("/admin/api/reservations/{id}/commit", post(commit))
        .fallback(fallback)
        .layer(middleware::from_fn(guard))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(SECURITY_HEADERS_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

/// Rejects non-loopback hosts, cross-origin requests and any Bearer credential.
/// `Origin: null` is not trusted by itself; login and writes additionally require a
/// page/session CSRF challenge, so an opaque-space request cannot act without one.
async fn guard(request: Request, next: Next) -> Response {
    if request.headers().contains_key(header::AUTHORIZATION) {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "agent tokens cannot authenticate the admin UI",
        );
    }
    if let Err(response) = request_boundary(request.headers()) {
        return *response;
    }
    next.run(request).await
}

fn request_boundary(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(host) = header_text(headers, header::HOST) else {
        return Err(reject(StatusCode::BAD_REQUEST, "missing Host header"));
    };
    if !host_is_loopback(host) {
        return Err(reject(
            StatusCode::FORBIDDEN,
            "admin is only available on a loopback host",
        ));
    }
    let Some(origin) = header_text(headers, header::ORIGIN) else {
        return Ok(());
    };
    if origin == "null" {
        return Ok(());
    }
    // Scheme, host and port must all match the request's Host header. There is no
    // wildcard or port-insensitive comparison; `null` is handled separately above.
    let expected = format!("http://{host}");
    if expected.eq_ignore_ascii_case(origin) {
        Ok(())
    } else {
        Err(reject(StatusCode::FORBIDDEN, "unexpected request origin"))
    }
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn index(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if session(&state, &headers, now_ms()).is_none() {
        return found("/admin/login");
    }
    html(
        StatusCode::OK,
        &index_html(&state.issue_login_challenge(now_ms())),
    )
}

async fn login_page(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if session(&state, &headers, now_ms()).is_some() {
        return found("/admin");
    }
    html(
        StatusCode::OK,
        &index_html(&state.issue_login_challenge(now_ms())),
    )
}

async fn app_css() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        APP_CSS,
    )
        .into_response()
}

async fn app_js() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
        .into_response()
}

async fn login_challenge(State(state): State<AdminState>) -> Response {
    let challenge = state.issue_login_challenge(now_ms());
    json_response(StatusCode::OK, json!({ "challenge": challenge }))
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
}

async fn login(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let now = now_ms();
    let Some(challenge) = header_text(&headers, header::HeaderName::from_static(CSRF_HEADER))
    else {
        return error_response(StatusCode::FORBIDDEN, "missing login page CSRF challenge");
    };
    if !state.login_challenge_valid(challenge, now) {
        return error_response(StatusCode::FORBIDDEN, "invalid login page CSRF challenge");
    }
    let consumed = match state.store.lock() {
        Ok(store) => store.consume_admin_login_token(&body.token, now),
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "state store lock poisoned",
            )
        }
    };
    if consumed.is_err() {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "login token is invalid, expired or already used",
        );
    }
    let session_id = auth::generate_secret();
    let session = AdminSession {
        csrf: auth::generate_secret(),
        expires_at: now + ADMIN_SESSION_TTL_MS,
    };
    let csrf = session.csrf.clone();
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.retain(|_, value| value.expires_at > now);
        sessions.insert(session_id.clone(), session);
    } else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session store lock poisoned",
        );
    }
    state.consume_login_challenge(challenge);
    let _ = store_call(&state, |store| {
        store.record_audit_event(&admin_event(
            "admin_login",
            "admin.login",
            None,
            None,
            "allowed",
            Some("one-time login token consumed"),
            now,
        ))
    });
    let mut response = json_response(
        StatusCode::OK,
        json!({ "authenticated": true, "mode": "web", "csrf": csrf }),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        set_cookie_header(SESSION_COOKIE, &session_id, true, ADMIN_SESSION_TTL_MS),
    );
    response
}

async fn logout(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let (session_id, _) = match write_guard(&state, &headers) {
        Ok(session) => session,
        Err(response) => return *response,
    };
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.remove(&session_id);
    }
    let mut response = json_response(StatusCode::OK, json!({ "ok": true }));
    response
        .headers_mut()
        .append(header::SET_COOKIE, clear_cookie_header(SESSION_COOKIE));
    response
}

async fn session_info(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match session(&state, &headers, now_ms()) {
        Some((_, session)) => json_response(
            StatusCode::OK,
            json!({ "authenticated": true, "mode": "web", "csrf": session.csrf }),
        ),
        None => json_response(
            StatusCode::OK,
            json!({ "authenticated": false, "mode": null, "csrf": null }),
        ),
    }
}

async fn overview(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    let result = store_call(&state, |store| {
        let sources = store.list_all_sources()?;
        let tokens = store.list_tokens()?;
        let pending = store.count_pending_approvals()?;
        let audit_total = store.count_audit_events_all()?;
        let quarantined = store.list_quarantined_reservations()?.len();
        let active_sources = sources
            .iter()
            .filter(|source| source.status == crate::SourceStatus::Active)
            .count();
        let active_tokens = tokens
            .iter()
            .filter(|token| token.status == "active")
            .count();
        Ok(json!({
            "sources": {
                "total": sources.len(),
                "active": active_sources,
                "archived": sources.len() - active_sources,
            },
            "tokens": {
                "total": tokens.len(),
                "active": active_tokens,
                "revoked": tokens.len() - active_tokens,
            },
            "pending_approvals": pending,
            "audit_total": audit_total,
            "quarantined_reservations": quarantined,
        }))
    });
    match result {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn list_sources(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    match store_call(&state, |store| {
        let sources = store.list_all_sources()?;
        let mut items = Vec::with_capacity(sources.len());
        for source in &sources {
            items.push(source_json(store, source)?);
        }
        Ok(json!({ "sources": items }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn source_detail(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    match store_call(&state, |store| source_detail_json(store, &id)) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct CreateSourceBody {
    id: String,
    environment: String,
}

async fn create_source(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateSourceBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let environment = match Environment::parse(body.environment.trim()) {
        Ok(environment) => environment,
        Err(error) => return map_store_error(error),
    };
    match store_call(&state, |store| {
        let source = store.create_source(&NewSource {
            id: body.id.trim().to_string(),
            driver: crate::Driver::Postgres,
            environment,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: Some("admin-web".to_string()),
        })?;
        record_admin_event(
            store,
            "admin_source_added",
            "source",
            Some(&source.id),
            None,
            "created source",
        )?;
        source_json(store, &source)
    }) {
        Ok(value) => json_response(StatusCode::CREATED, json!({ "source": value })),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct EnvironmentBody {
    environment: String,
}

async fn set_environment(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<EnvironmentBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let environment = match Environment::parse(body.environment.trim()) {
        Ok(environment) => environment,
        Err(error) => return map_store_error(error),
    };
    match store_call(&state, |store| {
        let source = store.set_source_environment(&id, environment)?;
        record_admin_event(
            store,
            "admin_source_environment_set",
            "source",
            Some(&source.id),
            None,
            "classified archived source environment",
        )?;
        source_json(store, &source)
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "source": value })),
        Err(response) => *response,
    }
}

async fn archive(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        let source = store.archive_source(&id, ADMIN_ACTOR, now_ms())?;
        source_json(store, &source)
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "source": value })),
        Err(response) => *response,
    }
}

async fn restore(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        let source = store.restore_source(&id, &state.provision_limits, ADMIN_ACTOR, now_ms())?;
        source_json(store, &source)
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "source": value })),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct CreateProfileBody {
    name: String,
    purpose: String,
    credential_ref: String,
    max_connections: Option<u32>,
    statement_timeout_ms: Option<u64>,
}

async fn create_profile(
    State(state): State<AdminState>,
    AxumPath(source_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<CreateProfileBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let purpose = match ProfilePurpose::parse(body.purpose.trim()) {
        Ok(purpose) => purpose,
        Err(error) => return map_store_error(error),
    };
    match store_call(&state, |store| {
        let profile = store.create_profile(&NewProfile {
            source_id: source_id.clone(),
            name: body.name.trim().to_string(),
            purpose,
            credential_ref: body.credential_ref.trim().to_string(),
            max_connections: body.max_connections,
            statement_timeout_ms: body.statement_timeout_ms,
        })?;
        record_admin_event(
            store,
            "admin_profile_added",
            "profile",
            Some(&source_id),
            None,
            "created connection profile",
        )?;
        Ok(profile_json(&profile))
    }) {
        Ok(value) => json_response(StatusCode::CREATED, json!({ "profile": value })),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct CreateRouteBody {
    tool: String,
    profile_id: i64,
    min_approval_policy: Option<String>,
}

async fn create_route(
    State(state): State<AdminState>,
    AxumPath(source_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<CreateRouteBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let tool = match Tool::parse(body.tool.trim()) {
        Ok(tool) => tool,
        Err(error) => return map_store_error(error),
    };
    let policy = match body
        .min_approval_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => default_policy(tool),
        Some(value) => match ApprovalPolicy::parse(value) {
            Ok(policy) => policy,
            Err(error) => return map_store_error(error),
        },
    };
    match store_call(&state, |store| {
        let route = store.create_route(&NewRoute {
            source_id: source_id.clone(),
            tool,
            profile_id: body.profile_id,
            min_approval_policy: policy,
        })?;
        record_admin_event(
            store,
            "admin_route_added",
            "route",
            Some(&source_id),
            None,
            "created tool route",
        )?;
        let profile = store.get_profile(route.profile_id)?;
        Ok(route_json(&route, &profile.name))
    }) {
        Ok(value) => json_response(StatusCode::CREATED, json!({ "route": value })),
        Err(response) => *response,
    }
}

async fn list_tokens(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    match store_call(&state, |store| {
        let tokens = store.list_tokens()?;
        let mut items = Vec::with_capacity(tokens.len());
        for token in &tokens {
            items.push(token_json(store, token)?);
        }
        Ok(json!({ "tokens": items }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct CreateTokenBody {
    agent: String,
}

async fn create_token(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateTokenBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        let issued = store.create_token(body.agent.trim())?;
        record_admin_event(
            store,
            "admin_token_issued",
            "token",
            None,
            Some(&issued.token_id),
            "issued agent token",
        )?;
        let token = token_json_by_id(store, &issued.token_id)?;
        Ok((token, issued.token))
    }) {
        Ok((token, plaintext)) => json_response(
            StatusCode::CREATED,
            json!({ "token": token, "plaintext": plaintext }),
        ),
        Err(response) => *response,
    }
}

async fn revoke_token(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        store.revoke_token(&id)?;
        record_admin_event(
            store,
            "admin_token_revoked",
            "token",
            None,
            Some(&id),
            "revoked agent token",
        )?;
        token_json_by_id(store, &id)
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "token": value })),
        Err(response) => *response,
    }
}

async fn list_grants(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    match store_call(&state, |store| {
        store.get_token_identity(&id)?;
        let grants = store.list_grants_for_token(&id)?;
        Ok(json!({ "grants": grants.iter().map(grant_json).collect::<Vec<_>>() }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct AddGrantBody {
    source_id: String,
    tool: String,
    policy: Option<String>,
}

async fn add_grant(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<AddGrantBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let tool = match Tool::parse(body.tool.trim()) {
        Ok(tool) => tool,
        Err(error) => return map_store_error(error),
    };
    let policy = match body
        .policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => None,
        Some(value) => match ApprovalPolicy::parse(value) {
            Ok(policy) => Some(policy),
            Err(error) => return map_store_error(error),
        },
    };
    match store_call(&state, |store| {
        let grant = store.create_grant(&NewGrant {
            token_id: id.clone(),
            source_id: body.source_id.clone(),
            tool,
            policy,
        })?;
        record_admin_event(
            store,
            "admin_grant_added",
            "grant",
            Some(&grant.source_id),
            Some(&grant.token_id),
            "added explicit source/tool grant",
        )?;
        Ok(grant_json(&grant))
    }) {
        Ok(value) => json_response(StatusCode::CREATED, json!({ "grant": value })),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct GrantQuery {
    source: String,
    tool: String,
}

async fn remove_grant(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Query(query): Query<GrantQuery>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let tool = match Tool::parse(query.tool.trim()) {
        Ok(tool) => tool,
        Err(error) => return map_store_error(error),
    };
    match store_call(&state, |store| {
        store.revoke_grant(&id, query.source.trim(), tool)?;
        record_admin_event(
            store,
            "admin_grant_revoked",
            "grant",
            Some(query.source.trim()),
            Some(&id),
            "removed explicit source/tool grant",
        )?;
        Ok(json!({ "ok": true }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn list_approvals(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    match store_call(&state, |store| {
        let approvals = store.list_pending_approvals(200)?;
        Ok(json!({
            "approvals": approvals.iter().map(approval_json).collect::<Vec<_>>(),
            "total": approvals.len(),
        }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn approve(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
) -> Response {
    decide(&state, &headers, id, true)
}

async fn deny(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
) -> Response {
    decide(&state, &headers, id, false)
}

fn decide(state: &AdminState, headers: &HeaderMap, id: i64, approve: bool) -> Response {
    if let Err(response) = write_guard(state, headers) {
        return *response;
    }
    let outcome = store_call(state, |store| {
        store.decide_approval(
            id,
            approve,
            ADMIN_ACTOR,
            now_ms(),
            state.approval_execution_ttl_ms,
        )
    });
    match outcome {
        Ok(DecisionOutcome::Approved) => {
            json_response(StatusCode::OK, json!({ "id": id, "status": "approved" }))
        }
        Ok(DecisionOutcome::Denied) => {
            json_response(StatusCode::OK, json!({ "id": id, "status": "denied" }))
        }
        Ok(DecisionOutcome::AlreadyDecided) => {
            error_response(StatusCode::CONFLICT, "approval was already decided")
        }
        Ok(DecisionOutcome::Expired) => error_response(StatusCode::CONFLICT, "approval expired"),
        Ok(DecisionOutcome::NotFound) => {
            error_response(StatusCode::NOT_FOUND, "approval not found")
        }
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct AuditQuery {
    agent: Option<String>,
    source: Option<String>,
    decision: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn audit(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200) as usize;
    let offset = query.offset.unwrap_or(0).max(0) as usize;
    let agent = non_empty(query.agent);
    let source = non_empty(query.source);
    let decision = non_empty(query.decision);
    match store_call(&state, |store| {
        let (items, total) = store.query_audit(
            agent.as_deref(),
            source.as_deref(),
            decision.as_deref(),
            limit,
            offset,
        )?;
        Ok(json!({
            "items": items.iter().map(audit_json).collect::<Vec<_>>(),
            "total": total,
            "limit": limit,
            "offset": offset,
        }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct ReservationQuery {
    status: Option<String>,
}

async fn list_reservations(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<ReservationQuery>,
) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "admin session is required");
    };
    let quarantined_only = query.status.as_deref() == Some("quarantined");
    match store_call(&state, |store| {
        let reservations = if quarantined_only {
            store.list_quarantined_reservations()?
        } else {
            store.list_recent_reservations(100)?
        };
        Ok(json!({
            "reservations": reservations.iter().map(reservation_json).collect::<Vec<_>>(),
        }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn release(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
) -> Response {
    settle(&state, &headers, id, ReservationStatus::Released)
}

async fn commit(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
) -> Response {
    settle(&state, &headers, id, ReservationStatus::Committed)
}

fn settle(state: &AdminState, headers: &HeaderMap, id: i64, target: ReservationStatus) -> Response {
    if let Err(response) = write_guard(state, headers) {
        return *response;
    }
    match store_call(state, |store| {
        let reservation = store.settle_reservation(id, target, Some("ADMIN_SETTLED"), now_ms())?;
        let event = match target {
            ReservationStatus::Committed => "admin_reservation_committed",
            _ => "admin_reservation_released",
        };
        record_admin_event(
            store,
            event,
            "reservation",
            Some(&reservation.parent_source_id),
            Some(&reservation.token_id),
            "settled quarantined provisioning reservation",
        )?;
        Ok(reservation_json(&reservation))
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "reservation": value })),
        Err(response) => *response,
    }
}

async fn fallback() -> Response {
    error_response(StatusCode::NOT_FOUND, "not found")
}

fn read_guard(state: &AdminState, headers: &HeaderMap) -> Option<(String, AdminSession)> {
    session(state, headers, now_ms())
}

fn write_guard(
    state: &AdminState,
    headers: &HeaderMap,
) -> Result<(String, AdminSession), Box<Response>> {
    let Some((session_id, session)) = session(state, headers, now_ms()) else {
        return Err(reject(
            StatusCode::UNAUTHORIZED,
            "admin session is required",
        ));
    };
    let Some(csrf) = header_text(headers, header::HeaderName::from_static(CSRF_HEADER)) else {
        return Err(reject(StatusCode::FORBIDDEN, "missing CSRF token"));
    };
    if !constant_time_eq(csrf, &session.csrf) {
        return Err(reject(StatusCode::FORBIDDEN, "invalid CSRF token"));
    }
    Ok((session_id, session))
}

fn session(state: &AdminState, headers: &HeaderMap, now: i64) -> Option<(String, AdminSession)> {
    let session_id = cookie(headers, SESSION_COOKIE)?;
    let mut sessions = state.sessions.lock().ok()?;
    match sessions.get(&session_id) {
        Some(session) if session.expires_at > now => Some((session_id, session.clone())),
        Some(_) => {
            sessions.remove(&session_id);
            None
        }
        None => None,
    }
}

fn store_call<T>(
    state: &AdminState,
    action: impl FnOnce(&Store) -> crate::error::Result<T>,
) -> Result<T, Box<Response>> {
    let store = match state.store.lock() {
        Ok(store) => store,
        Err(_) => {
            return Err(reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                "state store lock poisoned",
            ))
        }
    };
    action(&store).map_err(|error| Box::new(map_store_error(error)))
}

fn source_json(store: &Store, source: &Source) -> crate::error::Result<Value> {
    Ok(json!({
        "id": source.id,
        "driver": source.driver.as_str(),
        "environment": source.environment.as_str(),
        "status": source.status.as_str(),
        "parent_source_id": source.parent_source_id,
        "created_at": source.created_at,
        "updated_at": source.updated_at,
        "archived_at": source.archived_at,
        "profile_count": store.list_profiles(&source.id)?.len(),
        "grant_count": store.count_grants_for_source(&source.id)?,
    }))
}

fn source_detail_json(store: &Store, id: &str) -> crate::error::Result<Value> {
    let source = store.get_source(id)?;
    let profiles = store.list_profiles(id)?;
    let routes = store.list_routes(id)?;
    let names = profiles
        .iter()
        .map(|profile| (profile.id, profile.name.clone()))
        .collect::<HashMap<_, _>>();
    Ok(json!({
        "source": source_json(store, &source)?,
        "profiles": profiles.iter().map(profile_json).collect::<Vec<_>>(),
        "routes": routes
            .iter()
            .map(|route| route_json(route, names.get(&route.profile_id).map(String::as_str).unwrap_or("")))
            .collect::<Vec<_>>(),
    }))
}

fn profile_json(profile: &Profile) -> Value {
    json!({
        "id": profile.id,
        "source_id": profile.source_id,
        "name": profile.name,
        "purpose": profile.purpose.as_str(),
        "credential_ref": profile.credential_ref,
        "enabled": profile.enabled,
        "version": profile.version,
        "max_connections": profile.max_connections,
        "statement_timeout_ms": profile.statement_timeout_ms,
    })
}

fn route_json(route: &Route, profile_name: &str) -> Value {
    json!({
        "id": route.id,
        "source_id": route.source_id,
        "tool": route.tool.as_str(),
        "profile_id": route.profile_id,
        "profile_name": profile_name,
        "min_approval_policy": route.min_approval_policy.as_str(),
        "version": route.version,
    })
}

fn token_json(store: &Store, token: &crate::state::TokenRecord) -> crate::error::Result<Value> {
    Ok(json!({
        "id": token.id,
        "agent": token.agent,
        "status": token.status,
        "created_at": token.created_at,
        "revoked_at": token.revoked_at,
        "grants": store
            .list_grants_for_token(&token.id)?
            .iter()
            .map(grant_json)
            .collect::<Vec<_>>(),
    }))
}

fn token_json_by_id(store: &Store, id: &str) -> crate::error::Result<Value> {
    let token = store
        .list_tokens()?
        .into_iter()
        .find(|token| token.id == id)
        .ok_or_else(|| Error::NotFound(format!("token {id}")))?;
    token_json(store, &token)
}

fn grant_json(grant: &crate::state::Grant) -> Value {
    json!({
        "token_id": grant.token_id,
        "source_id": grant.source_id,
        "tool": grant.tool.as_str(),
        "policy": grant.policy.as_str(),
    })
}

fn approval_json(record: &crate::ApprovalRecord) -> Value {
    let draft = &record.draft;
    json!({
        "id": record.id,
        "token_id": draft.token_id,
        "agent": draft.agent,
        "source_id": draft.source_id,
        "source_environment": draft.source_environment.as_str(),
        "tool": draft.tool.as_str(),
        "sql": draft.sql,
        "params_json": draft.params_json,
        "request_digest": record.request_digest,
        "profile_id": draft.profile_id,
        "profile_version": draft.profile_version,
        "route_id": draft.route_id,
        "route_version": draft.route_version,
        "grant_policy": draft.grant_policy.as_str(),
        "route_min_policy": draft.route_min_policy.as_str(),
        "high_risk": draft.high_risk,
        "channel": record.channel.as_str(),
        "status": record.status.as_str(),
        "created_at": record.created_at,
        "expires_at": record.expires_at,
        "decided_at": record.decided_at,
        "decided_by": record.decided_by,
        "reason_code": record.reason_code,
        "reason": record.reason,
    })
}

fn audit_json(event: &AuditEvent) -> Value {
    json!({
        "ts": event.ts,
        "event": event.event,
        "approval_id": event.approval_id,
        "token_id": event.token_id,
        "agent": event.agent,
        "source_id": event.source_id,
        "tool": event.tool,
        "profile_id": event.profile_id,
        "profile_version": event.profile_version,
        "route_id": event.route_id,
        "route_version": event.route_version,
        "request_digest": event.request_digest,
        "channel": event.channel,
        "decision": event.decision,
        "actor": event.actor,
        "high_risk": event.high_risk,
        "reason_code": event.reason_code,
        "reason": event.reason,
        "duration_ms": event.duration_ms,
        "row_count": event.row_count,
        "error_code": event.error_code,
    })
}

fn reservation_json(reservation: &crate::state::QuotaReservation) -> Value {
    json!({
        "id": reservation.id,
        "token_id": reservation.token_id,
        "parent_source_id": reservation.parent_source_id,
        "database_name": reservation.database_name,
        "approval_id": reservation.approval_id,
        "status": reservation.status.as_str(),
        "reserved_at": reservation.reserved_at,
        "settled_at": reservation.settled_at,
        "reason_code": reservation.reason_code,
    })
}

fn admin_event(
    event: &str,
    tool: &str,
    source_id: Option<&str>,
    token_id: Option<&str>,
    decision: &str,
    reason: Option<&str>,
    now: i64,
) -> AuditEvent {
    AuditEvent {
        ts: now,
        event: event.to_string(),
        approval_id: None,
        token_id: token_id.map(str::to_string),
        agent: None,
        source_id: source_id.map(str::to_string),
        tool: tool.to_string(),
        profile_id: None,
        profile_version: None,
        route_id: None,
        route_version: None,
        request_digest: None,
        channel: Some("web".to_string()),
        decision: decision.to_string(),
        actor: ADMIN_ACTOR.to_string(),
        high_risk: false,
        reason_code: None,
        reason: reason.map(str::to_string),
        duration_ms: None,
        row_count: None,
        error_code: None,
    }
}

fn record_admin_event(
    store: &Store,
    event: &str,
    tool: &str,
    source_id: Option<&str>,
    token_id: Option<&str>,
    reason: &str,
) -> crate::error::Result<()> {
    store.record_audit_event(&admin_event(
        event,
        tool,
        source_id,
        token_id,
        "allowed",
        Some(reason),
        now_ms(),
    ))
}

fn default_policy(tool: Tool) -> ApprovalPolicy {
    if tool.is_write() || tool == Tool::CreateDatabase {
        ApprovalPolicy::Approval
    } else {
        ApprovalPolicy::Direct
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn index_html(challenge: &str) -> String {
    INDEX_HTML.replace("__MCPDB_LOGIN_CHALLENGE__", challenge)
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim();
    if host == "::1" {
        return true;
    }
    let (address, port) = if let Some(rest) = host.strip_prefix('[') {
        let Some((address, tail)) = rest.split_once(']') else {
            return false;
        };
        let port = if tail.is_empty() {
            None
        } else if let Some(port) = tail.strip_prefix(':') {
            Some(port)
        } else {
            return false;
        };
        (address, port)
    } else if let Some((address, port)) = host.rsplit_once(':') {
        if address.contains(':') {
            return false;
        }
        (address, Some(port))
    } else {
        (host, None)
    };
    if let Some(port) = port {
        if port.is_empty()
            || !port.chars().all(|character| character.is_ascii_digit())
            || port.parse::<u16>().map(|value| value == 0).unwrap_or(true)
        {
            return false;
        }
    }
    address.eq_ignore_ascii_case("localhost") || address == "127.0.0.1" || address == "::1"
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.trim().to_string())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn reject(status: StatusCode, message: &str) -> Box<Response> {
    Box::new(error_response(status, message))
}

fn error_response(status: StatusCode, message: &str) -> Response {
    json_response(status, json!({ "error": message }))
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn html(status: StatusCode, page: &str) -> Response {
    (status, Html(page.to_string())).into_response()
}

fn found(location: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

fn set_cookie_header(name: &str, value: &str, http_only: bool, max_age_ms: i64) -> HeaderValue {
    let mut cookie = format!(
        "{name}={value}; Path=/admin; SameSite=Strict; Max-Age={}",
        max_age_ms / 1000
    );
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn clear_cookie_header(name: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{name}=; Path=/admin; SameSite=Strict; Max-Age=0"))
        .unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn map_store_error(error: Error) -> Response {
    match error {
        Error::NotFound(message) => {
            error_response(StatusCode::NOT_FOUND, &format!("not found: {message}"))
        }
        Error::Conflict(message) => {
            error_response(StatusCode::CONFLICT, &format!("conflict: {message}"))
        }
        Error::InvalidInput(message) => error_response(
            StatusCode::BAD_REQUEST,
            &format!("invalid input: {message}"),
        ),
        Error::TokenRevoked => error_response(StatusCode::CONFLICT, "token is revoked"),
        Error::Unauthenticated => error_response(
            StatusCode::UNAUTHORIZED,
            "login token is invalid, expired or already used",
        ),
        Error::ResourceLimit(message) => {
            error_response(StatusCode::CONFLICT, &format!("resource limit: {message}"))
        }
        Error::ProvisionBlocked(message) => error_response(
            StatusCode::CONFLICT,
            &format!("provisioning blocked: {message}"),
        ),
        other => error_response(StatusCode::INTERNAL_SERVER_ERROR, &other.to_string()),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_hosts_are_accepted() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:8788",
            "localhost:8788",
            "[::1]:8788",
            "::1",
        ] {
            assert!(host_is_loopback(host), "{host}");
        }
        for host in [
            "example.com",
            "0.0.0.0:8788",
            "[::2]:8788",
            "10.0.0.1",
            "127.0.0.1:",
            "127.0.0.1:0",
            "127.0.0.1:not-a-port",
        ] {
            assert!(!host_is_loopback(host), "{host}");
        }
    }

    #[test]
    fn constant_time_comparison_matches_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
