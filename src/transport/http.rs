use std::io;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any_service, get};
use axum::{Json, Router};
use rmcp::serde_json::json;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use crate::auth::parse_bearer;
use crate::mcp::{AppState, McpDbServer, PeerElicitation};
use crate::provision::Provisioner;
use crate::{Config, Error, MetadataProvider, SqlExecutor, Store, DEFAULT_MAX_ROWS};

pub async fn run(
    config: Config,
    store: Arc<Mutex<Store>>,
    metadata: Arc<dyn MetadataProvider>,
    executor: Arc<dyn SqlExecutor>,
    provisioner: Arc<dyn Provisioner>,
) -> io::Result<()> {
    let listener = TcpListener::bind(&config.addr).await?;
    println!(
        "mcp-db {} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        config.addr
    );
    axum::serve(
        listener,
        app(AppState {
            store,
            metadata,
            executor,
            provisioner,
            provision_limits: config.provision_limits.clone(),
            rate_limiter: Arc::new(crate::limits::TokenRateLimiter::new(
                config.token_requests_per_minute,
                crate::limits::DEFAULT_TOKEN_RATE_WINDOW,
            )),
            elicitation: Arc::new(PeerElicitation),
            max_rows: DEFAULT_MAX_ROWS,
            approval_timeout_ms: config.approval_timeout_ms,
            approval_execution_ttl_ms: config.approval_execution_ttl_ms,
            sessions: Default::default(),
        }),
    )
    .await
}

pub fn app(state: AppState) -> Router {
    let service = StreamableHttpService::new(
        {
            let state = state.clone();
            move || Ok(McpDbServer::new(state.clone()))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_stateful_mode(true),
    );

    let mcp = Router::new()
        .route("/mcp", any_service(service))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(mcp)
        .with_state(state)
}

async fn healthz() -> Json<rmcp::serde_json::Value> {
    Json(json!({"status":"ok","service":"mcp-db"}))
}

async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let session_id = request
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer)
        .map(str::to_owned);
    let Some(token) = token else {
        return unauthorized("missing bearer token");
    };

    let identity = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => return internal_error("state store lock poisoned"),
        };
        store.authenticate(&token)
    };

    let identity = match identity {
        Ok(identity) => identity,
        Err(Error::Unauthenticated) => return unauthorized("unauthenticated"),
        Err(Error::TokenRevoked) => return unauthorized("token revoked"),
        Err(error) => return internal_error(&error.to_string()),
    };
    let token_id = identity.token_id.clone();

    if let Some(session_id) = &session_id {
        let sessions = match state.sessions.lock() {
            Ok(sessions) => sessions,
            Err(_) => return internal_error("session store lock poisoned"),
        };
        if let Some(bound_token) = sessions.get(session_id) {
            if bound_token != &token_id {
                return forbidden("session belongs to a different token");
            }
        }
    }

    request.extensions_mut().insert(identity);
    let response = next.run(request).await;

    if let Some(new_session) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    {
        if let Ok(mut sessions) = state.sessions.lock() {
            sessions.insert(new_session, token_id);
        }
    }
    if method == Method::DELETE {
        if let Some(session_id) = session_id {
            if let Ok(mut sessions) = state.sessions.lock() {
                sessions.remove(&session_id);
            }
        }
    }
    response
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": message }))).into_response()
}

fn unauthorized(message: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": message }))).into_response()
}

fn internal_error(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
        .into_response()
}
