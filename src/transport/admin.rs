//! Local-only web admin: one-time login token, in-process session, CSRF-checked forms.
//!
//! The admin listener is separate from the MCP listener and is expected to be bound or
//! published to the host loopback address only. Sessions live in process memory and are
//! lost on restart. Cookies are not marked `Secure` because the first version is served
//! over plain loopback HTTP; cross-host access requires TLS and is out of scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::auth;
use crate::provision::ProvisionLimits;
use crate::state::{ReservationStatus, Store};
use crate::Error;

/// One-time login token lifetime. The CLI prints the plaintext once.
pub const ADMIN_LOGIN_TTL_MS: i64 = 5 * 60 * 1000;
/// In-process admin session lifetime.
pub const ADMIN_SESSION_TTL_MS: i64 = 60 * 60 * 1000;
pub const SESSION_COOKIE: &str = "mcpdb_admin";
pub const CSRF_COOKIE: &str = "mcpdb_csrf";
const ADMIN_ACTOR: &str = "admin-web";
const AUDIT_PAGE_LIMIT: usize = 50;
const APPROVAL_PAGE_LIMIT: usize = 50;

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
        }
    }
}

pub fn admin_app(state: AdminState) -> Router {
    Router::new()
        .route("/admin/login", get(get_login).post(post_login))
        .route("/admin", get(dashboard))
        .route("/admin/approvals/{id}/approve", post(approve))
        .route("/admin/approvals/{id}/deny", post(deny))
        .route("/admin/sources/{id}/archive", post(archive))
        .route("/admin/sources/{id}/restore", post(restore))
        .route("/admin/reservations/{id}/release", post(release))
        .route("/admin/reservations/{id}/commit", post(commit))
        .with_state(state)
}

async fn get_login(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err(response) = precheck(&headers) {
        return *response;
    }
    if session(&state, &headers, now_ms()).is_some() {
        return see_other("/admin");
    }
    html(StatusCode::OK, &login_page())
}

async fn post_login(State(state): State<AdminState>, headers: HeaderMap, body: String) -> Response {
    if let Err(response) = precheck(&headers) {
        return *response;
    }
    let Some(token) = form_value(&body, "token") else {
        return text(StatusCode::UNAUTHORIZED, "login token is required");
    };
    let now = now_ms();
    let consumed = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => {
                return text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state store lock poisoned",
                )
            }
        };
        store.consume_admin_login_token(&token, now)
    };
    if consumed.is_err() {
        return text(
            StatusCode::UNAUTHORIZED,
            "login token is invalid, expired or already used",
        );
    }
    let session_id = auth::generate_secret();
    let session = AdminSession {
        csrf: auth::generate_secret(),
        expires_at: now + ADMIN_SESSION_TTL_MS,
    };
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.retain(|_, value| value.expires_at > now);
        sessions.insert(session_id.clone(), session);
    } else {
        return text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session store lock poisoned",
        );
    }
    let mut response = see_other("/admin");
    response.headers_mut().append(
        header::SET_COOKIE,
        set_cookie_header(SESSION_COOKIE, &session_id, true, ADMIN_SESSION_TTL_MS),
    );
    // Readable by the page so a future script client could echo it; forms also embed it.
    response.headers_mut().append(
        header::SET_COOKIE,
        set_cookie_header(
            CSRF_COOKIE,
            &csrf_for(&state, &session_id),
            false,
            ADMIN_SESSION_TTL_MS,
        ),
    );
    response
}

async fn dashboard(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err(response) = precheck(&headers) {
        return *response;
    }
    let Some((_, session)) = session(&state, &headers, now_ms()) else {
        return found("/admin/login");
    };
    match dashboard_page(&state, &session) {
        Ok(page) => html(StatusCode::OK, &page),
        Err(error) => map_store_error(error),
    }
}

async fn approve(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
    body: String,
) -> Response {
    decide(&state, &headers, &body, id, true)
}

async fn deny(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
    body: String,
) -> Response {
    decide(&state, &headers, &body, id, false)
}

fn decide(state: &AdminState, headers: &HeaderMap, body: &str, id: i64, approve: bool) -> Response {
    if let Err(response) = post_guard(state, headers, body, now_ms()) {
        return *response;
    }
    let now = now_ms();
    let outcome = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => {
                return text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state store lock poisoned",
                )
            }
        };
        store.decide_approval(
            id,
            approve,
            ADMIN_ACTOR,
            now,
            state.approval_execution_ttl_ms,
        )
    };
    match outcome {
        Ok(crate::DecisionOutcome::Approved | crate::DecisionOutcome::Denied) => {
            see_other("/admin")
        }
        Ok(crate::DecisionOutcome::AlreadyDecided) => {
            text(StatusCode::CONFLICT, "approval was already decided")
        }
        Ok(crate::DecisionOutcome::Expired) => text(StatusCode::CONFLICT, "approval expired"),
        Ok(crate::DecisionOutcome::NotFound) => text(StatusCode::NOT_FOUND, "approval not found"),
        Err(error) => map_store_error(error),
    }
}

async fn archive(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(response) = post_guard(&state, &headers, &body, now_ms()) {
        return *response;
    }
    let outcome = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => {
                return text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state store lock poisoned",
                )
            }
        };
        store.archive_source(&id, ADMIN_ACTOR, now_ms())
    };
    match outcome {
        Ok(_) => see_other("/admin"),
        Err(error) => map_store_error(error),
    }
}

async fn restore(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(response) = post_guard(&state, &headers, &body, now_ms()) {
        return *response;
    }
    let outcome = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => {
                return text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state store lock poisoned",
                )
            }
        };
        store.restore_source(&id, &state.provision_limits, ADMIN_ACTOR, now_ms())
    };
    match outcome {
        Ok(_) => see_other("/admin"),
        Err(error) => map_store_error(error),
    }
}

async fn release(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
    body: String,
) -> Response {
    settle(&state, &headers, &body, id, ReservationStatus::Released)
}

async fn commit(
    State(state): State<AdminState>,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
    body: String,
) -> Response {
    settle(&state, &headers, &body, id, ReservationStatus::Committed)
}

fn settle(
    state: &AdminState,
    headers: &HeaderMap,
    body: &str,
    id: i64,
    target: ReservationStatus,
) -> Response {
    if let Err(response) = post_guard(state, headers, body, now_ms()) {
        return *response;
    }
    let outcome = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => {
                return text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state store lock poisoned",
                )
            }
        };
        store.settle_reservation(id, target, Some("ADMIN_SETTLED"), now_ms())
    };
    match outcome {
        Ok(_) => see_other("/admin"),
        Err(error) => map_store_error(error),
    }
}

/// Rejects non-loopback hosts, cross-origin writes and any Bearer credential.
/// Agent tokens never authenticate the admin surface.
fn precheck(headers: &HeaderMap) -> Result<(), Box<Response>> {
    if headers.contains_key(header::AUTHORIZATION) {
        return Err(reject(
            StatusCode::UNAUTHORIZED,
            "agent tokens cannot authenticate the admin UI",
        ));
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(reject(StatusCode::BAD_REQUEST, "missing Host header"));
    };
    if !host_is_loopback(host) {
        return Err(reject(
            StatusCode::FORBIDDEN,
            "admin is only available on a loopback host",
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let expected = format!("http://{host}");
        let matches = origin
            .to_str()
            .map(|value| value == expected)
            .unwrap_or(false);
        if !matches {
            return Err(reject(StatusCode::FORBIDDEN, "unexpected request origin"));
        }
    }
    Ok(())
}

fn reject(status: StatusCode, message: &str) -> Box<Response> {
    Box::new(text(status, message))
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

fn post_guard(
    state: &AdminState,
    headers: &HeaderMap,
    body: &str,
    now: i64,
) -> Result<AdminSession, Box<Response>> {
    precheck(headers)?;
    let Some((_, session)) = session(state, headers, now) else {
        return Err(reject(
            StatusCode::UNAUTHORIZED,
            "admin session is required",
        ));
    };
    let Some(csrf) = form_value(body, "csrf") else {
        return Err(reject(StatusCode::FORBIDDEN, "missing CSRF token"));
    };
    if !constant_time_eq(&csrf, &session.csrf) {
        return Err(reject(StatusCode::FORBIDDEN, "invalid CSRF token"));
    }
    Ok(session)
}

fn csrf_for(state: &AdminState, session_id: &str) -> String {
    state
        .sessions
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).map(|session| session.csrf.clone()))
        .unwrap_or_default()
}

fn dashboard_page(state: &AdminState, session: &AdminSession) -> Result<String, Error> {
    let (pending, sources, reservations, audit) = {
        let store = state
            .store
            .lock()
            .map_err(|_| Error::InvalidInput("state store lock poisoned".to_string()))?;
        (
            store.list_pending_approvals(APPROVAL_PAGE_LIMIT)?,
            store.list_all_sources()?,
            store.list_quarantined_reservations()?,
            store.list_recent_audit(AUDIT_PAGE_LIMIT)?,
        )
    };
    let csrf = escape_html(&session.csrf);
    let mut page = String::new();
    page.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    page.push_str("<title>mcp-db admin</title></head><body>");
    page.push_str("<h1>mcp-db admin</h1>");

    page.push_str("<h2>Pending approvals</h2>");
    if pending.is_empty() {
        page.push_str("<p>No pending approvals.</p>");
    } else {
        page.push_str("<table><tr><th>id</th><th>tool</th><th>source</th><th>agent</th>");
        page.push_str("<th>created</th><th>decision</th></tr>");
        for record in &pending {
            page.push_str("<tr>");
            page.push_str(&format!("<td>{}</td>", record.id));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(record.draft.tool.as_str())
            ));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(&record.draft.source_id)
            ));
            page.push_str(&format!("<td>{}</td>", escape_html(&record.draft.agent)));
            page.push_str(&format!("<td>{}</td>", record.created_at));
            page.push_str("<td>");
            for (action, label) in [("approve", "Approve"), ("deny", "Deny")] {
                page.push_str(&format!(
                    "<form method=\"post\" action=\"/admin/approvals/{}/{action}\">\
                     <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
                     <button type=\"submit\">{label}</button></form>",
                    record.id
                ));
            }
            page.push_str("</td></tr>");
        }
        page.push_str("</table>");
    }

    page.push_str("<h2>Sources</h2>");
    if sources.is_empty() {
        page.push_str("<p>No sources.</p>");
    } else {
        page.push_str("<table><tr><th>id</th><th>environment</th><th>status</th>");
        page.push_str("<th>action</th></tr>");
        for source in &sources {
            page.push_str("<tr>");
            page.push_str(&format!("<td>{}</td>", escape_html(&source.id)));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(source.environment.as_str())
            ));
            page.push_str(&format!("<td>{}</td>", escape_html(source.status.as_str())));
            page.push_str("<td>");
            let action = match source.status {
                crate::SourceStatus::Active => ("archive", "Archive"),
                crate::SourceStatus::Archived => ("restore", "Restore"),
            };
            page.push_str(&format!(
                "<form method=\"post\" action=\"/admin/sources/{}/{}\">\
                 <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
                 <button type=\"submit\">{}</button></form>",
                escape_html(&source.id),
                action.0,
                action.1
            ));
            page.push_str("</td></tr>");
        }
        page.push_str("</table>");
    }

    page.push_str("<h2>Quarantined reservations</h2>");
    if reservations.is_empty() {
        page.push_str("<p>No quarantined reservations.</p>");
    } else {
        page.push_str("<table><tr><th>id</th><th>database</th><th>token</th>");
        page.push_str("<th>reason</th><th>settle</th></tr>");
        for reservation in &reservations {
            page.push_str("<tr>");
            page.push_str(&format!("<td>{}</td>", reservation.id));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(&reservation.database_name)
            ));
            page.push_str(&format!("<td>{}</td>", escape_html(&reservation.token_id)));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(reservation.reason_code.as_deref().unwrap_or("-"))
            ));
            page.push_str("<td>");
            for (path, label) in [("release", "Release"), ("commit", "Commit")] {
                page.push_str(&format!(
                    "<form method=\"post\" action=\"/admin/reservations/{}/{path}\">\
                     <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
                     <button type=\"submit\">{label}</button></form>",
                    reservation.id
                ));
            }
            page.push_str("</td></tr>");
        }
        page.push_str("</table>");
    }

    page.push_str("<h2>Recent audit</h2>");
    if audit.is_empty() {
        page.push_str("<p>No audit events.</p>");
    } else {
        page.push_str(
            "<table><tr><th>ts</th><th>event</th><th>actor</th><th>tool</th>\
             <th>source</th><th>decision</th><th>reason</th></tr>",
        );
        for event in &audit {
            page.push_str("<tr>");
            page.push_str(&format!("<td>{}</td>", event.ts));
            page.push_str(&format!("<td>{}</td>", escape_html(&event.event)));
            page.push_str(&format!("<td>{}</td>", escape_html(&event.actor)));
            page.push_str(&format!("<td>{}</td>", escape_html(&event.tool)));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(event.source_id.as_deref().unwrap_or("-"))
            ));
            page.push_str(&format!("<td>{}</td>", escape_html(&event.decision)));
            page.push_str(&format!(
                "<td>{}</td>",
                escape_html(event.reason_code.as_deref().unwrap_or("-"))
            ));
            page.push_str("</tr>");
        }
        page.push_str("</table>");
    }
    page.push_str("</body></html>");
    Ok(page)
}

fn login_page() -> String {
    "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
     <title>mcp-db admin login</title></head><body>\
     <h1>mcp-db admin</h1>\
     <p>Run <code>mcp-db admin-login-token</code> and paste the one-time token.</p>\
     <form method=\"post\" action=\"/admin/login\">\
     <label>Login token <input type=\"password\" name=\"token\" autocomplete=\"off\"></label>\
     <button type=\"submit\">Sign in</button></form></body></html>"
        .to_string()
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        return rest
            .split_once(']')
            .map(|(address, _)| address == "::1")
            .unwrap_or(false);
    }
    if host == "::1" {
        return true;
    }
    matches!(
        host.split(':').next().unwrap_or_default(),
        "127.0.0.1" | "localhost"
    )
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

fn form_value(body: &str, key: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (percent_decode(name) == key).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn text(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_string(),
    )
        .into_response()
}

fn html(status: StatusCode, page: &str) -> Response {
    (status, Html(page.to_string())).into_response()
}

fn found(location: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

fn see_other(location: &str) -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
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

fn map_store_error(error: Error) -> Response {
    match error {
        Error::NotFound(message) => text(StatusCode::NOT_FOUND, &format!("not found: {message}")),
        Error::Conflict(message) => text(StatusCode::CONFLICT, &format!("conflict: {message}")),
        Error::InvalidInput(message) => text(
            StatusCode::BAD_REQUEST,
            &format!("invalid input: {message}"),
        ),
        Error::Unauthenticated => text(
            StatusCode::UNAUTHORIZED,
            "login token is invalid, expired or already used",
        ),
        other => text(StatusCode::INTERNAL_SERVER_ERROR, &other.to_string()),
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
        ] {
            assert!(host_is_loopback(host), "{host}");
        }
        for host in ["example.com", "0.0.0.0:8788", "[::2]:8788", "10.0.0.1"] {
            assert!(!host_is_loopback(host), "{host}");
        }
    }

    #[test]
    fn html_escape_covers_attribute_characters() {
        assert_eq!(
            escape_html("<script>\"x\"&'</script>"),
            "&lt;script&gt;&quot;x&quot;&amp;&#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn form_values_decode_percent_escapes() {
        assert_eq!(
            form_value("csrf=abc%2Ddef&token=x", "csrf").as_deref(),
            Some("abc-def")
        );
        assert_eq!(form_value("other=1", "csrf"), None);
    }

    #[test]
    fn constant_time_comparison_matches_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
