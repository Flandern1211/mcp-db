//! Local-only web admin: one-time login token, in-process session, CSRF-checked JSON API.
//!
//! The admin listener is separate from the MCP listener and is expected to be bound or
//! published to the host loopback address only. Sessions live in process memory and are
//! lost on restart. Cookies are not marked `Secure` because the first version is served
//! over plain loopback HTTP; cross-host access requires TLS and is out of scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequest, Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approval::AuditEvent;
use crate::auth;
use crate::connection_test::ConnectionTester;
use crate::credentials::{CredentialResolver, CredentialSink};
use crate::provision::ProvisionLimits;
use crate::state::{
    ApprovalPolicy, Environment, NewGrant, NewProfile, NewRoute, NewSource, Profile,
    ProfilePurpose, ProfileUpdate, ReservationStatus, Route, RouteUpdate, Source, Store, Tool,
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
const SECURITY_HEADERS_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

const INDEX_HTML: &str = include_str!("admin_assets/index.html");
const DAISYUI_CSS: &str = include_str!("admin_assets/daisyui.css");
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
    /// Present only when an encrypted credential store is configured; the admin
    /// form never receives an existing DSN back, it can only write a new one.
    pub credentials: Option<Arc<dyn CredentialSink>>,
    /// Resolves stored references for "test connection" on saved profiles.
    pub resolver: Option<Arc<dyn CredentialResolver>>,
    /// Executes the read-only connectivity probe.
    pub tester: Option<Arc<dyn ConnectionTester>>,
    pub sessions: Arc<Mutex<HashMap<String, AdminSession>>>,
    pub login_challenges: Arc<Mutex<HashMap<String, i64>>>,
}

impl AdminState {
    pub fn new(
        store: Arc<Mutex<Store>>,
        provision_limits: ProvisionLimits,
        approval_execution_ttl_ms: i64,
        credentials: Option<Arc<dyn CredentialSink>>,
        resolver: Option<Arc<dyn CredentialResolver>>,
        tester: Option<Arc<dyn ConnectionTester>>,
    ) -> Self {
        Self {
            store,
            provision_limits,
            approval_execution_ttl_ms,
            credentials,
            resolver,
            tester,
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
        .route("/admin/assets/daisyui.css", get(daisyui_css))
        .route("/admin/assets/app.css", get(app_css))
        .route("/admin/assets/app.js", get(app_js))
        .route("/admin/api/login-challenge", get(login_challenge))
        .route("/admin/api/login", post(login))
        .route("/admin/api/logout", post(logout))
        .route("/admin/api/session", get(session_info))
        .route("/admin/api/overview", get(overview))
        .route("/admin/api/sources", get(list_sources).post(create_source))
        .route(
            "/admin/api/sources/{id}",
            get(source_detail).delete(delete_source),
        )
        .route("/admin/api/sources/{id}/environment", post(set_environment))
        .route("/admin/api/sources/{id}/archive", post(archive))
        .route("/admin/api/sources/{id}/restore", post(restore))
        .route("/admin/api/sources/{id}/profiles", post(create_profile))
        .route(
            "/admin/api/sources/{id}/profiles/{profile_id}",
            axum::routing::put(update_profile).delete(delete_profile),
        )
        .route(
            "/admin/api/sources/{id}/profiles/{profile_id}/test",
            post(test_profile),
        )
        .route("/admin/api/connection-test", post(test_connection))
        .route("/admin/api/sources/{id}/routes", post(create_route))
        .route(
            "/admin/api/sources/{id}/routes/{route_id}",
            axum::routing::put(update_route).delete(delete_route),
        )
        .route("/admin/api/tokens", get(list_tokens).post(create_token))
        .route("/admin/api/tokens/{id}/revoke", post(revoke_token))
        .route(
            "/admin/api/tokens/{id}/grants",
            get(list_grants).post(add_grant).delete(remove_grant),
        )
        .route(
            "/admin/api/tokens/{id}/sources/{source_id}/grants",
            axum::routing::put(replace_source_grants),
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
///
/// A trusted reverse proxy may forward the admin UI to a public HTTPS entry point.
/// When the request carries consistent proxy headers (`X-Forwarded-Proto: https`
/// plus `X-Forwarded-Host`), the boundary is evaluated against the forwarded
/// values instead of the loopback requirement. Without them the direct-access
/// loopback rule applies unchanged.
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

/// Effective request origin as seen through optional proxy headers.
///
/// Returns `None` when the headers are absent or inconsistent: a proxy must set
/// both `X-Forwarded-Proto: https` and `X-Forwarded-Host`; one without the other
/// is treated as absent rather than half-trusted.
fn forwarded_origin(headers: &HeaderMap) -> Option<(bool, &'_ str)> {
    let proto = header_text(headers, HeaderName::from_static("x-forwarded-proto"))?;
    if !proto.eq_ignore_ascii_case("https") {
        return None;
    }
    let host = header_text(headers, HeaderName::from_static("x-forwarded-host"))?;
    if host.trim().is_empty() || host.contains(',') {
        return None;
    }
    Some((true, host))
}

fn request_boundary(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let (https, host) = match forwarded_origin(headers) {
        Some((https, host)) => (https, host),
        None => {
            let host = header_text(headers, header::HOST)
                .ok_or_else(|| reject(StatusCode::BAD_REQUEST, "missing Host header"))?;
            if !host_is_loopback(host) {
                return Err(reject(
                    StatusCode::FORBIDDEN,
                    "admin is only available on a loopback host",
                ));
            }
            (false, host)
        }
    };
    let Some(origin) = header_text(headers, header::ORIGIN) else {
        return Ok(());
    };
    if origin == "null" {
        return Ok(());
    }
    // Scheme, host and port must all match the effective request host. There is
    // no wildcard or port-insensitive comparison; `null` is handled separately above.
    let scheme = if https { "https" } else { "http" };
    let expected = format!("{scheme}://{host}");
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

async fn daisyui_css() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        DAISYUI_CSS,
    )
        .into_response()
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
        return error_response(StatusCode::FORBIDDEN, "登录页面已过期，请刷新页面后重试。");
    };
    if !state.login_challenge_valid(challenge, now) {
        return error_response(StatusCode::FORBIDDEN, "登录页面已过期，请刷新页面后重试。");
    }
    let auth_reason = match state.store.lock() {
        Ok(store) => {
            if store
                .consume_admin_login_token(body.token.trim(), now)
                .is_ok()
            {
                Some("one-time login token consumed")
            } else if store.authenticate_admin_key(body.token.trim(), now).is_ok() {
                Some("permanent admin key authenticated")
            } else {
                None
            }
        }
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "state store lock poisoned",
            )
        }
    };
    let Some(auth_reason) = auth_reason else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "管理员密钥无效、已撤销，或登录令牌已过期/已使用",
        );
    };
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
            Some(auth_reason),
            now,
        ))
    });
    let mut response = json_response(
        StatusCode::OK,
        json!({ "authenticated": true, "mode": "web", "csrf": csrf }),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        set_cookie_header(SESSION_COOKIE, &session_id, true, ADMIN_SESSION_TTL_MS, {
            forwarded_origin(&headers).is_some()
        }),
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
    response.headers_mut().append(
        header::SET_COOKIE,
        clear_cookie_header(SESSION_COOKIE, forwarded_origin(&headers).is_some()),
    );
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
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
    };
    match store_call(&state, |store| {
        let sources = store.list_all_sources()?;
        let mut items = Vec::with_capacity(sources.len());
        for source in &sources {
            let mut item = source_json(store, source)?;
            let tools = Tool::SOURCE_TOOLS.into_iter().map(|tool| {
                Ok(match store.grant_route(source, tool)? {
                    Ok(route) => json!({ "tool": tool.as_str(), "min_policy": route.min_approval_policy.as_str() }),
                    Err(reason) => json!({ "tool": tool.as_str(), "reason": reason }),
                })
            }).collect::<crate::error::Result<Vec<_>>>()?;
            item["grant_tools"] = json!(tools);
            items.push(item);
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
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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

async fn delete_source(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let (source, references) = match store_call(&state, |store| {
        // Snapshot before the rows are gone; the response is rebuilt from it.
        let references = store
            .list_profiles(&id)?
            .into_iter()
            .map(|profile| profile.credential_ref)
            .collect::<Vec<_>>();
        let source = store.delete_source(&id, ADMIN_ACTOR, now_ms())?;
        record_admin_event(
            store,
            "admin_source_deleted",
            "source",
            Some(&id),
            None,
            "deleted source control-plane records",
        )?;
        Ok((source, references))
    }) {
        Ok(result) => result,
        Err(response) => return *response,
    };
    // Credential cleanup runs after the store commit: a failed cleanup must not
    // roll back or fail the already-completed delete, so it degrades to warnings.
    let mut warnings = Vec::new();
    if let Some(sink) = state.credentials.as_ref() {
        for reference in &references {
            let in_use = match store_call(&state, |store| {
                store.count_profiles_with_credential(reference)
            }) {
                Ok(count) => count,
                Err(_) => {
                    warnings.push(format!(
                        "凭证引用 {reference} 的引用检查失败，请手动确认是否可以删除。"
                    ));
                    continue;
                }
            };
            if in_use > 0 {
                continue;
            }
            if let Err(error) = sink.remove(reference) {
                warnings.push(format!(
                    "托管凭证 {reference} 未能从凭证库删除：{error}；请检查凭证库后手动清理。"
                ));
            }
        }
    }
    json_response(
        StatusCode::OK,
        json!({
            "deleted": true,
            "source": {
                "id": source.id,
                "driver": source.driver.as_str(),
                "environment": source.environment.as_str(),
            },
            "warnings": warnings,
        }),
    )
}

#[derive(Deserialize)]
struct CreateProfileBody {
    name: String,
    purpose: String,
    /// Preferred path from the web form: a plaintext DSN that is encrypted
    /// immediately and never echoed back.
    dsn: Option<String>,
    /// Legacy/CLI path for callers that manage the credential store themselves.
    credential_ref: Option<String>,
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
    let name = body.name.trim().to_string();
    let dsn = body
        .dsn
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let credential_ref = match dsn {
        Some(dsn) => {
            if let Err(message) = validate_postgres_dsn(dsn) {
                return error_response(StatusCode::BAD_REQUEST, message);
            }
            let Some(sink) = state.credentials.as_ref() else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "凭证库未配置，服务端无法保存数据库连接串",
                );
            };
            let reference = managed_credential_ref(&source_id, purpose);
            if let Err(error) = sink.store(&reference, dsn) {
                return map_store_error(error);
            }
            reference
        }
        None => match body
            .credential_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(reference) => reference.to_string(),
            None => {
                return error_response(StatusCode::BAD_REQUEST, "请输入数据库连接串");
            }
        },
    };
    match store_call(&state, |store| {
        let profile = store.create_profile(&NewProfile {
            source_id: source_id.clone(),
            name,
            purpose,
            credential_ref,
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

/// Reject malformed or non-PostgreSQL DSNs before they reach the credential store.
fn validate_postgres_dsn(dsn: &str) -> Result<(), &'static str> {
    if !(dsn.starts_with("postgres://") || dsn.starts_with("postgresql://")) {
        return Err("连接串必须以 postgres:// 或 postgresql:// 开头");
    }
    let config: tokio_postgres::Config = dsn
        .parse()
        .map_err(|_| "连接串格式不正确，请检查用户名、密码和主机地址")?;
    if config.get_dbname().is_none() {
        return Err("连接串必须包含数据库名");
    }
    Ok(())
}

/// Server-owned reference: the browser never has to invent or remember one.
/// The random suffix prevents a failed/repeated create from overwriting the
/// credential still used by an existing profile with the same name.
fn managed_credential_ref(source_id: &str, purpose: ProfilePurpose) -> String {
    let suffix = &auth::generate_secret()[..12];
    if purpose == ProfilePurpose::Provision {
        format!("keyring:provision/managed/{source_id}/{suffix}")
    } else {
        format!("keyring:managed/{source_id}/{suffix}")
    }
}

#[derive(Deserialize)]
struct UpdateProfileBody {
    name: String,
    enabled: Option<bool>,
    dsn: Option<String>,
    max_connections: Option<u32>,
    statement_timeout_ms: Option<u64>,
}

async fn update_profile(
    State(state): State<AdminState>,
    AxumPath((source_id, profile_id)): AxumPath<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<UpdateProfileBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    let name = body.name.trim().to_string();
    let dsn = body
        .dsn
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let credential_ref = match dsn {
        Some(dsn) => {
            if let Err(message) = validate_postgres_dsn(dsn) {
                return error_response(StatusCode::BAD_REQUEST, message);
            }
            let Some(sink) = state.credentials.as_ref() else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "凭证库未配置，服务端无法保存数据库连接串",
                );
            };
            let purpose = match store_call(&state, |store| {
                let profile = store.get_profile(profile_id)?;
                if profile.source_id != source_id {
                    return Err(Error::NotFound(format!("profile {profile_id}")));
                }
                Ok(profile.purpose)
            }) {
                Ok(purpose) => purpose,
                Err(response) => return *response,
            };
            let reference = managed_credential_ref(&source_id, purpose);
            if let Err(error) = sink.store(&reference, dsn) {
                return map_store_error(error);
            }
            Some(reference)
        }
        None => None,
    };
    match store_call(&state, |store| {
        let current = store.get_profile(profile_id)?;
        if current.source_id != source_id {
            return Err(Error::NotFound(format!("profile {profile_id}")));
        }
        let profile = store.update_profile(
            &source_id,
            profile_id,
            &ProfileUpdate {
                name,
                enabled: body.enabled.unwrap_or(current.enabled),
                credential_ref,
                max_connections: body.max_connections,
                statement_timeout_ms: body.statement_timeout_ms,
            },
        )?;
        record_admin_event(
            store,
            "admin_profile_updated",
            "profile",
            Some(&source_id),
            Some(&profile_id.to_string()),
            "updated connection profile",
        )?;
        Ok(profile_json(&profile))
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "profile": value })),
        Err(response) => *response,
    }
}

async fn delete_profile(
    State(state): State<AdminState>,
    AxumPath((source_id, profile_id)): AxumPath<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        store.delete_profile(&source_id, profile_id)?;
        record_admin_event(
            store,
            "admin_profile_deleted",
            "profile",
            Some(&source_id),
            Some(&profile_id.to_string()),
            "deleted connection profile",
        )?;
        Ok(json!({ "deleted": true }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct TestConnectionBody {
    dsn: String,
}

async fn test_connection(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<TestConnectionBody>,
) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
    };
    let dsn = body.dsn.trim();
    if dsn.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "请输入数据库连接串");
    }
    probe_connection(&state, dsn).await
}

async fn test_profile(
    State(state): State<AdminState>,
    AxumPath((source_id, profile_id)): AxumPath<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
    };
    let credential_ref = match store_call(&state, |store| {
        let profile = store.get_profile(profile_id)?;
        if profile.source_id != source_id {
            return Err(Error::NotFound(format!("profile {profile_id}")));
        }
        Ok(profile.credential_ref)
    }) {
        Ok(reference) => reference,
        Err(response) => return *response,
    };
    let Some(resolver) = state.resolver.as_ref() else {
        return error_response(StatusCode::BAD_REQUEST, "凭证库未配置，无法解析连接串");
    };
    let dsn = match resolver.resolve(&credential_ref) {
        Ok(dsn) => dsn,
        Err(error) => return map_store_error(error),
    };
    probe_connection(&state, &dsn).await
}

async fn probe_connection(state: &AdminState, dsn: &str) -> Response {
    let Some(tester) = state.tester.as_ref() else {
        return error_response(StatusCode::BAD_REQUEST, "连接测试器未配置");
    };
    let started = std::time::Instant::now();
    match tester.test(dsn).await {
        Ok(info) => json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "database": info.database,
                "user": info.user,
                "serverVersion": info.server_version,
                "latencyMs": started.elapsed().as_millis() as u64,
            }),
        ),
        Err(Error::InvalidInput(message)) => error_response(StatusCode::BAD_REQUEST, &message),
        Err(error) => error_response(StatusCode::BAD_GATEWAY, &format!("连接失败：{error}")),
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

#[derive(Deserialize)]
struct UpdateRouteBody {
    profile_id: i64,
    min_approval_policy: Option<String>,
}

async fn update_route(
    State(state): State<AdminState>,
    AxumPath((source_id, route_id)): AxumPath<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<UpdateRouteBody>,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        let current = store.get_route_by_id(route_id)?;
        if current.source_id != source_id {
            return Err(Error::NotFound(format!("route {route_id}")));
        }
        let policy = match body
            .min_approval_policy
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => ApprovalPolicy::parse(value)?,
            None => current.min_approval_policy,
        };
        let route = store.update_route(
            &source_id,
            route_id,
            &RouteUpdate {
                profile_id: body.profile_id,
                min_approval_policy: policy,
            },
        )?;
        record_admin_event(
            store,
            "admin_route_updated",
            "route",
            Some(&source_id),
            Some(&route_id.to_string()),
            "updated tool route",
        )?;
        let profile = store.get_profile(route.profile_id)?;
        Ok(route_json(&route, &profile.name))
    }) {
        Ok(value) => json_response(StatusCode::OK, json!({ "route": value })),
        Err(response) => *response,
    }
}

async fn delete_route(
    State(state): State<AdminState>,
    AxumPath((source_id, route_id)): AxumPath<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = write_guard(&state, &headers) {
        return *response;
    }
    match store_call(&state, |store| {
        store.delete_route(&source_id, route_id)?;
        record_admin_event(
            store,
            "admin_route_deleted",
            "route",
            Some(&source_id),
            Some(&route_id.to_string()),
            "deleted tool route",
        )?;
        Ok(json!({ "deleted": true }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn list_tokens(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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
#[serde(deny_unknown_fields)]
struct SourceGrantEntry {
    tool: String,
    policy: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceSourceGrantsBody {
    expected: Vec<SourceGrantEntry>,
    grants: Vec<SourceGrantEntry>,
}

async fn replace_source_grants(
    State(state): State<AdminState>,
    AxumPath((id, source_id)): AxumPath<(String, String)>,
    request: Request,
) -> Response {
    if let Err(response) = write_guard(&state, request.headers()) {
        return *response;
    }
    let Json(body) = match Json::<ReplaceSourceGrantsBody>::from_request(request, &state).await {
        Ok(body) => body,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "必须提供 expected 和 grants 两组授权，每项包含 tool 和 policy。",
            )
        }
    };
    let parse = |items: Vec<SourceGrantEntry>| -> crate::error::Result<Vec<_>> {
        items
            .into_iter()
            .map(|entry| {
                Ok((
                    Tool::parse(&entry.tool)?,
                    ApprovalPolicy::parse(&entry.policy)?,
                ))
            })
            .collect()
    };
    match store_call(&state, |store| {
        let updated = store.replace_source_grants(
            &id,
            &source_id,
            &parse(body.expected)?,
            &parse(body.grants)?,
            ADMIN_ACTOR,
            now_ms(),
        )?;
        Ok(json!({ "grants": updated.iter().map(grant_json).collect::<Vec<_>>() }))
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
        store.revoke_grant(&id, query.source.trim(), tool, ADMIN_ACTOR)?;
        Ok(json!({ "ok": true }))
    }) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(response) => *response,
    }
}

async fn list_approvals(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(_) = read_guard(&state, &headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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
        Ok(DecisionOutcome::AlreadyDecided) => error_response(
            StatusCode::CONFLICT,
            "这个审批已经被处理过了，请刷新后查看结果。",
        ),
        Ok(DecisionOutcome::Expired) => {
            error_response(StatusCode::CONFLICT, "这个审批已过期，请重新发起。")
        }
        Ok(DecisionOutcome::NotFound) => error_response(
            StatusCode::NOT_FOUND,
            "没有找到这个审批，可能已被处理或删除。",
        ),
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
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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
        return error_response(StatusCode::UNAUTHORIZED, "请先登录管理台，再执行这个操作。");
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
    error_response(StatusCode::NOT_FOUND, "没有找到对应的页面。")
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
            "请先登录管理台，再执行这个操作。",
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
        "credential_configured": true,
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

fn set_cookie_header(
    name: &str,
    value: &str,
    http_only: bool,
    max_age_ms: i64,
    secure: bool,
) -> HeaderValue {
    let mut cookie = format!(
        "{name}={value}; Path=/admin; SameSite=Strict; Max-Age={}",
        max_age_ms / 1000
    );
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    // Sessions must never ride over plain HTTP once a TLS entry point exists.
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn clear_cookie_header(name: &str, secure: bool) -> HeaderValue {
    let mut cookie = format!("{name}=; Path=/admin; SameSite=Strict; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn map_store_error(error: Error) -> Response {
    match error {
        Error::NotFound(_) => error_response(
            StatusCode::NOT_FOUND,
            "没有找到对应的数据，可能已被删除，请刷新页面后重试。",
        ),
        Error::Conflict(message) => {
            error_response(StatusCode::CONFLICT, &humanize_conflict(&message))
        }
        Error::InvalidInput(message) => {
            error_response(StatusCode::BAD_REQUEST, &humanize_input(&message))
        }
        Error::TokenRevoked => {
            error_response(StatusCode::CONFLICT, "该令牌已被撤销，无法继续操作。")
        }
        Error::Unauthenticated => {
            error_response(StatusCode::UNAUTHORIZED, "登录已失效，请重新登录。")
        }
        Error::ResourceLimit(message) => error_response(
            StatusCode::CONFLICT,
            &format!("已达到资源限制：{}", humanize_conflict(&message)),
        ),
        Error::ProvisionBlocked(message) => error_response(
            StatusCode::CONFLICT,
            &format!("建库操作没有完成：{}", humanize_conflict(&message)),
        ),
        _ => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "服务暂时无法完成这个操作，请稍后重试；如果持续失败，请查看服务日志。",
        ),
    }
}

fn has_chinese(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

fn humanize_conflict(message: &str) -> String {
    if has_chinese(message) {
        return message.to_string();
    }
    match message {
        "constraint violation" => {
            "这次修改与已有数据冲突。可能是名称重复，或这条记录刚刚被其他操作改过，请刷新后重试。"
                .to_string()
        }
        "source is not active" => "这个数据源已归档或停用，当前操作不可用。".to_string(),
        "source is not archived" => "只有已归档的数据源才能执行这个操作。".to_string(),
        "source environment can only change while archived" => {
            "只有归档状态的数据源才能修改环境类型。".to_string()
        }
        "archived source has no enabled profile" => {
            "这个数据源还没有启用的连接配置，请先启用一个再恢复。".to_string()
        }
        "reservation is not reserved" | "reservation is not in a settleable state" => {
            "这条建库预留已经处理过或状态已变化，请刷新后查看。".to_string()
        }
        "database name is already reserved" => {
            "这个数据库名已经被预留，请换一个名字或等待预留释放。".to_string()
        }
        "reservation was settled concurrently" => {
            "这条建库预留刚刚被其他操作处理过，请刷新后查看。".to_string()
        }
        "approval belongs to another token" => {
            "这个审批不属于当前令牌，无法在这里处理。".to_string()
        }
        "token is revoked" => "该令牌已被撤销，无法继续操作。".to_string(),
        _ => "当前数据状态不允许这个操作，请刷新页面后重试。".to_string(),
    }
}

fn humanize_input(message: &str) -> String {
    if has_chinese(message) {
        return message.to_string();
    }
    if message.contains("must not be empty") {
        return "名称不能为空。".to_string();
    }
    if message.contains("must start with a lowercase ASCII letter") {
        return "名称必须以小写字母开头。".to_string();
    }
    if message.contains("may contain only lowercase letters, digits") {
        return "名称只能包含小写字母、数字、下划线和连字符。".to_string();
    }
    if message.contains("must be ") && message.contains(" characters") {
        return "名称长度不符合要求，请检查后重试。".to_string();
    }
    if message.contains("credential_ref") {
        return "凭证引用无效，请重新填写数据库连接串。".to_string();
    }
    match message {
        "cannot grant access to an archived source" => "已归档的数据源不能添加授权。".to_string(),
        "list_sources is available to every authenticated token" => {
            "list_sources 是所有有效令牌自带的基础工具，不需要单独授权。".to_string()
        }
        "production sources cannot grant write tools" => {
            "生产数据源不能授予写入或结构变更权限。".to_string()
        }
        "archived source has no enabled profile" => {
            "这个数据源还没有启用的连接配置，请先启用一个再恢复。".to_string()
        }
        "archived source has no grants to restore" => {
            "这个数据源还没有任何授权，恢复前请先添加授权。".to_string()
        }
        "cannot add profile to an archived source" => {
            "已归档的数据源不能新增连接配置，请先恢复数据源。".to_string()
        }
        "reservation does not match the database name" => {
            "预留记录与数据库名不匹配，请重新发起建库。".to_string()
        }
        _ => "填写内容不符合要求，请检查后重试。".to_string(),
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

    #[test]
    fn forwarded_https_origin_is_accepted_with_matching_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-host"),
            HeaderValue::from_static("db.example.net"),
        );
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://db.example.net"),
        );
        assert!(request_boundary(&headers).is_ok());
    }

    #[test]
    fn forwarded_origin_requires_both_proto_and_host() {
        // Proto without host: treated as a direct request, so the public host
        // is rejected by the loopback rule instead of being half-trusted.
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("db.example.net"));
        assert!(request_boundary(&headers).is_err());

        // Host without proto: same.
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-host"),
            HeaderValue::from_static("db.example.net"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("db.example.net"));
        assert!(request_boundary(&headers).is_err());

        // Comma-chained hosts (multi-proxy) are not accepted either.
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-host"),
            HeaderValue::from_static("a.example.net, b.example.net"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("a.example.net"));
        assert!(request_boundary(&headers).is_err());
    }

    #[test]
    fn forwarded_origin_still_rejects_cross_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-host"),
            HeaderValue::from_static("db.example.net"),
        );
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example.net"),
        );
        assert!(request_boundary(&headers).is_err());

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://db.example.net"),
        );
        assert!(request_boundary(&headers).is_err());
    }

    #[test]
    fn direct_access_still_requires_loopback() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("43.1.2.3:8788"));
        assert!(request_boundary(&headers).is_err());
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8788"));
        assert!(request_boundary(&headers).is_ok());
    }

    #[test]
    fn validates_postgres_dsn_before_storing() {
        assert!(validate_postgres_dsn("postgres://user:pw@127.0.0.1:5432/app").is_ok());
        assert!(validate_postgres_dsn("postgresql://user@db.internal/app").is_ok());
        assert!(validate_postgres_dsn("mysql://user:pw@127.0.0.1/app").is_err());
        assert!(validate_postgres_dsn("postgres://user:pw@127.0.0.1").is_err());
        assert!(validate_postgres_dsn("not a dsn").is_err());
    }

    #[test]
    fn managed_refs_are_server_owned_and_provision_namespaced() {
        let query = managed_credential_ref("app", ProfilePurpose::Query);
        let provision = managed_credential_ref("app", ProfilePurpose::Provision);
        assert!(query.starts_with("keyring:managed/app/"));
        assert!(provision.starts_with("keyring:provision/managed/app/"));
        assert!(query.len() <= 128 && provision.len() <= 128);
        assert!(crate::state::model::validate_credential_ref(&query).is_ok());
        assert!(crate::state::model::validate_credential_ref(&provision).is_ok());
    }
}
