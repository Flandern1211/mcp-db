use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use mcp_db::transport::http::app;
use mcp_db::{
    AppState, ApprovalPolicy, Driver, ElicitationGate, ElicitationOutcome, EncryptedFileResolver,
    Environment, MetadataProvider, MutationOutcome, NewGrant, NewProfile, NewRoute, NewSource,
    PeerElicitation, PostgresExecutor, PostgresMetadata, Profile, ProfilePurpose, ProvisionLimits,
    ProvisionOutcome, Provisioner, QueryOutcome, SqlExecutor, Store, TokenRateLimiter, Tool,
    UnavailableExecutor, UnavailableProvisioner, DEFAULT_MAX_ROWS,
};
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

#[derive(Default)]
struct MockSqlExecutor {
    mutations: AtomicUsize,
    block: bool,
    started: tokio::sync::Notify,
    released: Arc<AtomicUsize>,
}

struct ReleaseOnDrop(Arc<AtomicUsize>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct FixedElicitation(ElicitationOutcome);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProvisionMode {
    Success,
    NotStarted,
    Unknown,
}

struct MockProvisioner {
    calls: AtomicUsize,
    mode: ProvisionMode,
}

impl MockProvisioner {
    fn new(mode: ProvisionMode) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mode,
        }
    }
}

#[async_trait]
impl Provisioner for MockProvisioner {
    async fn create(
        &self,
        _profile: &Profile,
        spec: &mcp_db::ProvisionSpec,
    ) -> Result<ProvisionOutcome, mcp_db::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            ProvisionMode::Success => Ok(ProvisionOutcome {
                database: spec.database.clone(),
            }),
            ProvisionMode::NotStarted => {
                Err(mcp_db::Error::ExecutionNotStarted("mock".to_string()))
            }
            ProvisionMode::Unknown => Err(mcp_db::Error::ExecutionUnknown),
        }
    }
}

#[async_trait]
impl ElicitationGate for FixedElicitation {
    async fn ask(
        &self,
        _context: &RequestContext<RoleServer>,
        _prompt: &str,
    ) -> ElicitationOutcome {
        self.0
    }
}

#[async_trait]
impl SqlExecutor for MockSqlExecutor {
    async fn query(
        &self,
        _profile: &Profile,
        _sql: &str,
        _params: &[Value],
        _max_rows: usize,
    ) -> Result<QueryOutcome, mcp_db::Error> {
        if self.block {
            let _guard = ReleaseOnDrop(self.released.clone());
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(QueryOutcome {
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            returned_rows: 0,
            truncated: false,
        })
    }

    async fn mutate(
        &self,
        _profile: &Profile,
        _sql: &str,
        _params: &[Value],
    ) -> Result<MutationOutcome, mcp_db::Error> {
        self.mutations.fetch_add(1, Ordering::SeqCst);
        if self.block {
            let _guard = ReleaseOnDrop(self.released.clone());
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(MutationOutcome { row_count: 1 })
    }

    async fn explain(
        &self,
        _profile: &Profile,
        _explain_sql: &str,
    ) -> Result<Value, mcp_db::Error> {
        Ok(json!([]))
    }
}

#[derive(Default)]
struct MockMetadata {
    list_calls: AtomicUsize,
    describe_calls: AtomicUsize,
    explain_calls: AtomicUsize,
}

#[async_trait]
impl MetadataProvider for MockMetadata {
    async fn list_tables(
        &self,
        _profile: &Profile,
        schema: Option<&str>,
    ) -> Result<Value, mcp_db::Error> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!([{
            "schema": schema.unwrap_or("public"),
            "name": "users",
            "kind": "table"
        }]))
    }

    async fn describe_table(
        &self,
        _profile: &Profile,
        table: &str,
        _schema: Option<&str>,
    ) -> Result<Value, mcp_db::Error> {
        self.describe_calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({
            "table": table,
            "columns": [{"name": "id", "type": "bigint", "nullable": false}]
        }))
    }

    async fn explain(
        &self,
        _profile: &Profile,
        _sql: &str,
        _params: &[Value],
    ) -> Result<Value, mcp_db::Error> {
        self.explain_calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!([{"Plan": {"Node Type": "Seq Scan"}}]))
    }
}

#[tokio::test]
async fn rate_limit_is_shared_across_sessions_and_does_not_consume_approvals() {
    let (_dir, mut state, token, metadata) = setup();
    state.rate_limiter = Arc::new(TokenRateLimiter::new(1, std::time::Duration::from_secs(60)));
    let router = app(state.clone());
    let first = init_session(router.clone(), &token).await;
    let second = init_session(router.clone(), &token).await;
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&token),
            Some(&first),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&token),
            Some(&second),
        ),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["code"], "RATE_LIMITED");
    assert_eq!(metadata.list_calls.load(Ordering::SeqCst), 0);
    assert!(state
        .store
        .lock()
        .unwrap()
        .list_pending_approvals(10)
        .unwrap()
        .is_empty());
    let other = state
        .store
        .lock()
        .unwrap()
        .create_token("agent-b")
        .unwrap()
        .token;
    let other_session = init_session(router.clone(), &other).await;
    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&other),
            Some(&other_session),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
}

#[tokio::test]
async fn mcp_cancellation_drops_query_and_records_unknown_write() {
    for write in [false, true] {
        let (_dir, mut state, token, _) = setup();
        let mock = Arc::new(MockSqlExecutor {
            block: true,
            ..Default::default()
        });
        state.executor = mock.clone();
        add_metadata_access(&state, &token);
        if write {
            let store = state.store.lock().unwrap();
            let profile = store
                .create_profile(&NewProfile {
                    source_id: "a".to_string(),
                    name: "write".to_string(),
                    purpose: ProfilePurpose::Mutate,
                    credential_ref: "keyring:test".to_string(),
                    max_connections: Some(1),
                    statement_timeout_ms: Some(1000),
                })
                .unwrap();
            store
                .create_route(&NewRoute {
                    source_id: "a".to_string(),
                    tool: Tool::MutateSql,
                    profile_id: profile.id,
                    min_approval_policy: ApprovalPolicy::Direct,
                })
                .unwrap();
            store
                .create_grant(&NewGrant {
                    token_id: store.authenticate(&token).unwrap().token_id,
                    source_id: "a".to_string(),
                    tool: Tool::MutateSql,
                    policy: Some(ApprovalPolicy::Direct),
                })
                .unwrap();
        }
        let router = app(state);
        let session = init_session(router.clone(), &token).await;
        let task = tokio::spawn(send(
            router.clone(),
            request(
                Method::POST,
                "/mcp",
                if write { MUTATE_INSERT } else { QUERY_SQL },
                Some(&token),
                Some(&session),
            ),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), mock.started.notified())
            .await
            .unwrap();
        let cancel = json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params": {
            "requestId": if write {12} else {8}, "reason":"test cancellation"
        }})
        .to_string();
        let _ = send(
            router,
            request(Method::POST, "/mcp", &cancel, Some(&token), Some(&session)),
        )
        .await;
        let (_, _, body) = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mock.released.load(Ordering::SeqCst), 1);
        if write {
            let connection = rusqlite::Connection::open(_dir.path().join("state.db")).unwrap();
            let unknown: i64 = connection.query_row(
                "SELECT COUNT(*) FROM audit_events WHERE event = 'direct_unknown' AND decision = 'unknown' AND error_code = 'EXECUTION_UNKNOWN'",
                [], |row| row.get(0),
            ).unwrap();
            assert_eq!(unknown, 1);
        }
        let code = body["result"]["structuredContent"]["code"].as_str();
        // The SDK may suppress the cancelled response; when delivered, its outcome stays conservative.
        if code.is_some() {
            assert_eq!(
                code,
                Some(if write {
                    "EXECUTION_UNKNOWN"
                } else {
                    "REQUEST_CANCELLED"
                })
            );
        }
    }
}

fn setup() -> (TempDir, AppState, String, Arc<MockMetadata>) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let token = store.create_token("agent-a").expect("issue token");
    let metadata = Arc::new(MockMetadata::default());
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        metadata: metadata.clone(),
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
    (dir, state, token.token, metadata)
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
    if text.lines().all(|line| {
        line.trim().is_empty()
            || line.starts_with(':')
            || line.starts_with("event:")
            || line.starts_with("id:")
            || line.starts_with("retry:")
            || line.trim() == "data:"
    }) {
        return Value::Null;
    }
    serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("JSON or SSE body: {text:?}"))
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
const LIST_SOURCES: &str = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}"#;
const LIST_TABLES: &str = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_tables","arguments":{"source":"a"}}}"#;
const DESCRIBE_TABLE: &str = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"describe_table","arguments":{"source":"a","table":"users"}}}"#;
const EXPLAIN_QUERY: &str = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"explain_query","arguments":{"source":"a","sql":"SELECT 1"}}}"#;
const QUERY_SQL: &str = r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"query_sql","arguments":{"source":"a","sql":"SELECT 1"}}}"#;
const QUERY_SQL_CROSS: &str = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"query_sql","arguments":{"source":"a","sql":"CREATE TABLE t (id bigint)"}}}"#;
const QUERY_SQL_PARAM: &str = r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"query_sql","arguments":{"source":"a","sql":"SELECT name FROM users WHERE id = $1","params":[1]}}}"#;
const QUERY_SQL_ALL: &str = r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"query_sql","arguments":{"source":"a","sql":"SELECT id, name FROM users ORDER BY id"}}}"#;
const MUTATE_INSERT: &str = r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"mutate_sql","arguments":{"source":"a","sql":"INSERT INTO users (id, name) VALUES ($1, $2)","params":[99,"mcp"]}}}"#;
const QUERY_VERIFY: &str = r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"query_sql","arguments":{"source":"a","sql":"SELECT name FROM users WHERE id = $1","params":[99]}}}"#;
const DDL_CREATE: &str = r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"execute_ddl","arguments":{"source":"a","sql":"CREATE TABLE mcp_e2e (id bigint)"}}}"#;
const DDL_DROP: &str = r#"{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"execute_ddl","arguments":{"source":"a","sql":"DROP TABLE users"}}}"#;
const LIST_TABLES_B: &str = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"list_tables","arguments":{"source":"b"}}}"#;

async fn init_session(router: axum::Router, token: &str) -> String {
    let (status, headers, _) = send(
        router.clone(),
        request(Method::POST, "/mcp", INITIALIZE, Some(token), None),
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
        router,
        request(
            Method::POST,
            "/mcp",
            INITIALIZED,
            Some(token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    session
}

fn add_metadata_access(state: &AppState, token: &str) {
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
    let profile = store
        .create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "query".to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(5),
            statement_timeout_ms: Some(1_000),
        })
        .expect("create profile");
    let token_id = store.authenticate(token).expect("auth").token_id;
    for tool in [
        Tool::ListTables,
        Tool::DescribeTable,
        Tool::ExplainQuery,
        Tool::QuerySql,
    ] {
        store
            .create_route(&NewRoute {
                source_id: source.id.clone(),
                tool,
                profile_id: profile.id,
                min_approval_policy: ApprovalPolicy::Direct,
            })
            .expect("create route");
        store
            .create_grant(&NewGrant {
                token_id: token_id.clone(),
                source_id: source.id.clone(),
                tool,
                policy: None,
            })
            .expect("create grant");
    }
}

#[tokio::test]
async fn mcp_requires_a_bearer_token() {
    let (_dir, state, _token, _metadata) = setup();
    let (status, _, _) = send(
        app(state),
        request(Method::POST, "/mcp", INITIALIZE, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_lists_only_granted_sources_and_binds_sessions_to_tokens() {
    let (_dir, state, token, _metadata) = setup();
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

#[tokio::test]
async fn metadata_tools_authorize_before_provider_and_expose_structured_results() {
    let (_dir, state, token, metadata) = setup();
    add_metadata_access(&state, &token);
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;

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
    let names = body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"list_sources"));
    assert!(names.contains(&"list_tables"));
    assert!(names.contains(&"describe_table"));
    assert!(names.contains(&"explain_query"));
    assert!(names.contains(&"query_sql"));

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_TABLES,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["tables"][0]["name"],
        "users"
    );
    assert_eq!(metadata.list_calls.load(Ordering::SeqCst), 1);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            DESCRIBE_TABLE,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["description"]["columns"][0]["name"],
        "id"
    );
    assert_eq!(metadata.describe_calls.load(Ordering::SeqCst), 1);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            EXPLAIN_QUERY,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "METADATA_UNAVAILABLE"
    );
    assert_eq!(metadata.explain_calls.load(Ordering::SeqCst), 0);

    {
        let store = state.store.lock().expect("store lock");
        store
            .create_source(&NewSource {
                id: "b".to_string(),
                driver: Driver::Postgres,
                environment: Environment::NonProduction,
                parent_source_id: None,
                created_token_id: None,
                created_agent: None,
                created_origin: None,
            })
            .expect("create second source");
    }
    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_TABLES_B,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "TOOL_NOT_GRANTED"
    );
    assert_eq!(metadata.list_calls.load(Ordering::SeqCst), 1);
    assert_eq!(metadata.explain_calls.load(Ordering::SeqCst), 0);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            QUERY_SQL,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "METADATA_UNAVAILABLE"
    );

    let (status, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            QUERY_SQL_CROSS,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(body["result"]["structuredContent"]["code"], "SQL_REJECTED");
}

#[tokio::test]
#[ignore = "requires MCPDB_TEST_PG_DSN and a read-only PostgreSQL test account"]
async fn encrypted_credentials_flow_through_mcp_to_postgres() {
    let dsn = env::var("MCPDB_TEST_PG_DSN")
        .expect("MCPDB_TEST_PG_DSN must point at a controlled PostgreSQL test database");
    let dir = TempDir::new().expect("temp dir");
    let key = [9_u8; 32];
    let key_hex = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let key_file = dir.path().join("master.key");
    fs::write(&key_file, key_hex).expect("write key");
    let store_path = dir.path().join("credentials.enc");
    let entries = BTreeMap::from([("keyring:mcp-db/test/query".to_string(), dsn)]);
    EncryptedFileResolver::seal(&store_path, &key, &entries).expect("seal");
    let resolver = EncryptedFileResolver::open(&store_path, &key_file).expect("open credentials");

    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let token = store.create_token("agent-a").expect("issue token");
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
        .expect("create source");
    let profile = store
        .create_profile(&NewProfile {
            source_id: "a".to_string(),
            name: "query".to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(1_000),
        })
        .expect("create profile");
    let token_id = store.authenticate(&token.token).expect("auth").token_id;
    for tool in [Tool::ListTables, Tool::DescribeTable] {
        store
            .create_route(&NewRoute {
                source_id: "a".to_string(),
                tool,
                profile_id: profile.id,
                min_approval_policy: ApprovalPolicy::Direct,
            })
            .expect("create route");
        store
            .create_grant(&NewGrant {
                token_id: token_id.clone(),
                source_id: "a".to_string(),
                tool,
                policy: None,
            })
            .expect("create grant");
    }

    let resolver = Arc::new(resolver);
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        metadata: Arc::new(PostgresMetadata::new(resolver.clone())),
        executor: Arc::new(PostgresExecutor::new(resolver)),
        provisioner: Arc::new(UnavailableProvisioner("test".to_string())),
        provision_limits: ProvisionLimits::default(),
        elicitation: Arc::new(PeerElicitation),
        rate_limiter: Arc::new(TokenRateLimiter::default()),
        max_rows: DEFAULT_MAX_ROWS,
        approval_timeout_ms: 120_000,
        approval_execution_ttl_ms: 60_000,
        sessions: Default::default(),
    };
    let router = app(state);
    let session = init_session(router.clone(), &token.token).await;

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_TABLES,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tables = body["result"]["structuredContent"]["tables"]
        .as_array()
        .expect("tables");
    assert!(tables.iter().any(|table| table["name"] == "users"));

    let (status, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            DESCRIBE_TABLE,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["description"]["table"],
        "users"
    );
    assert!(
        body["result"]["structuredContent"]["description"]["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .any(|column| column["name"] == "id")
    );
}

#[tokio::test]
#[ignore = "requires MCPDB_TEST_PG_DSN and a controlled PostgreSQL test account"]
async fn encrypted_credentials_flow_through_mcp_sql_tools() {
    let dsn = env::var("MCPDB_TEST_PG_DSN")
        .expect("MCPDB_TEST_PG_DSN must point at a controlled PostgreSQL test database");
    // The shared fixture must be repeatable: reset rows and DDL left by earlier runs.
    let cleanup_dsn = dsn.clone();
    reset_sql_tools_fixture(&dsn).await;
    let dir = TempDir::new().expect("temp dir");
    let key = [11_u8; 32];
    let key_hex = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let key_file = dir.path().join("master.key");
    fs::write(&key_file, key_hex).expect("write key");
    let store_path = dir.path().join("credentials.enc");
    let entries = BTreeMap::from([("keyring:mcp-db/test/query".to_string(), dsn)]);
    EncryptedFileResolver::seal(&store_path, &key, &entries).expect("seal");
    let resolver =
        Arc::new(EncryptedFileResolver::open(&store_path, &key_file).expect("open credentials"));

    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let token = store.create_token("agent-a").expect("issue token");
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
    let token_id = store.authenticate(&token.token).expect("auth").token_id;

    let query_profile = store
        .create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "query".to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(2_000),
        })
        .expect("create query profile");
    let mutate_profile = store
        .create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "mutate".to_string(),
            purpose: ProfilePurpose::Mutate,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(2_000),
        })
        .expect("create mutate profile");
    let schema_profile = store
        .create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "schema".to_string(),
            purpose: ProfilePurpose::Schema,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(2_000),
        })
        .expect("create schema profile");

    for (tool, profile_id) in [
        (Tool::QuerySql, query_profile.id),
        (Tool::ExplainQuery, query_profile.id),
        (Tool::MutateSql, mutate_profile.id),
        (Tool::ExecuteDdl, schema_profile.id),
    ] {
        store
            .create_route(&NewRoute {
                source_id: source.id.clone(),
                tool,
                profile_id,
                min_approval_policy: ApprovalPolicy::Direct,
            })
            .expect("create route");
        store
            .create_grant(&NewGrant {
                token_id: token_id.clone(),
                source_id: source.id.clone(),
                tool,
                policy: Some(ApprovalPolicy::Direct),
            })
            .expect("create grant");
    }

    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        metadata: Arc::new(PostgresMetadata::new(resolver.clone())),
        executor: Arc::new(PostgresExecutor::new(resolver)),
        provisioner: Arc::new(UnavailableProvisioner("test".to_string())),
        provision_limits: ProvisionLimits::default(),
        elicitation: Arc::new(PeerElicitation),
        rate_limiter: Arc::new(TokenRateLimiter::default()),
        max_rows: 1,
        approval_timeout_ms: 120_000,
        approval_execution_ttl_ms: 60_000,
        sessions: Default::default(),
    };
    let router = app(state);
    let session = init_session(router.clone(), &token.token).await;

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            QUERY_SQL_PARAM,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["rows"][0]["name"],
        "alice"
    );
    assert_eq!(body["result"]["structuredContent"]["truncated"], false);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            QUERY_SQL_ALL,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["structuredContent"]["rowCount"], 2);
    assert_eq!(body["result"]["structuredContent"]["returnedRows"], 1);
    assert_eq!(body["result"]["structuredContent"]["truncated"], true);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            MUTATE_INSERT,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["structuredContent"]["rowCount"], 1);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            QUERY_VERIFY,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["rows"][0]["name"],
        "mcp"
    );

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            DDL_CREATE,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["structuredContent"]["highRisk"], false);

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            EXPLAIN_QUERY,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["result"]["structuredContent"]["plan"].is_array());

    let (status, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            QUERY_SQL_CROSS,
            Some(&token.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["structuredContent"]["code"], "SQL_REJECTED");
    reset_sql_tools_fixture(&cleanup_dsn).await;
}

async fn reset_sql_tools_fixture(dsn: &str) {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .expect("fixture connection");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute("DELETE FROM users WHERE id = 99; DROP TABLE IF EXISTS mcp_e2e")
        .await
        .expect("reset sql tools fixture");
}

#[tokio::test]
async fn approval_policy_and_high_risk_fail_closed_before_executor() {
    let (_dir, state, token, _metadata) = setup();
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
        let mutate = store
            .create_profile(&NewProfile {
                source_id: source.id.clone(),
                name: "mutate".to_string(),
                purpose: ProfilePurpose::Mutate,
                credential_ref: "keyring:mcp-db/test/query".to_string(),
                max_connections: Some(1),
                statement_timeout_ms: Some(1_000),
            })
            .expect("create mutate profile");
        let schema = store
            .create_profile(&NewProfile {
                source_id: source.id.clone(),
                name: "schema".to_string(),
                purpose: ProfilePurpose::Schema,
                credential_ref: "keyring:mcp-db/test/query".to_string(),
                max_connections: Some(1),
                statement_timeout_ms: Some(1_000),
            })
            .expect("create schema profile");
        let token_id = store.authenticate(&token).expect("auth").token_id;
        for (tool, profile_id, policy) in [
            (Tool::MutateSql, mutate.id, ApprovalPolicy::Approval),
            (Tool::ExecuteDdl, schema.id, ApprovalPolicy::Direct),
        ] {
            store
                .create_route(&NewRoute {
                    source_id: source.id.clone(),
                    tool,
                    profile_id,
                    min_approval_policy: ApprovalPolicy::Direct,
                })
                .expect("create route");
            store
                .create_grant(&NewGrant {
                    token_id: token_id.clone(),
                    source_id: source.id.clone(),
                    tool,
                    policy: Some(policy),
                })
                .expect("create grant");
        }
    }

    let router = app(state);
    let session = init_session(router.clone(), &token).await;

    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            MUTATE_INSERT,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_REQUIRED"
    );

    let (status, _, body) = send(
        router,
        request(Method::POST, "/mcp", DDL_DROP, Some(&token), Some(&session)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_REQUIRED"
    );
    assert_eq!(body["result"]["structuredContent"]["highRisk"], true);
}

#[tokio::test]
async fn admin_approval_is_consumed_once_via_retry() {
    let (_dir, mut state, token, _metadata) = setup();
    let mock = Arc::new(MockSqlExecutor::default());
    state.executor = mock.clone();

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
        let profile = store
            .create_profile(&NewProfile {
                source_id: source.id.clone(),
                name: "mutate".to_string(),
                purpose: ProfilePurpose::Mutate,
                credential_ref: "keyring:mcp-db/test/query".to_string(),
                max_connections: Some(1),
                statement_timeout_ms: Some(1_000),
            })
            .expect("create profile");
        let route = store
            .create_route(&NewRoute {
                source_id: source.id.clone(),
                tool: Tool::MutateSql,
                profile_id: profile.id,
                min_approval_policy: ApprovalPolicy::Direct,
            })
            .expect("create route");
        let token_id = store.authenticate(&token).expect("auth").token_id;
        store
            .create_grant(&NewGrant {
                token_id,
                source_id: source.id,
                tool: Tool::MutateSql,
                policy: Some(ApprovalPolicy::Approval),
            })
            .expect("create grant");
        let _ = route;
    }

    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (status, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            MUTATE_INSERT,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_REQUIRED"
    );
    let approval_id = body["result"]["structuredContent"]["approvalId"]
        .as_i64()
        .expect("approval id");
    assert_eq!(mock.mutations.load(Ordering::SeqCst), 0);

    {
        let store = state.store.lock().expect("store lock");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(
            store
                .decide_approval(approval_id, true, "admin", now, 60_000)
                .expect("decide"),
            mcp_db::DecisionOutcome::Approved
        );
    }

    let retry = format!(
        r#"{{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{{"name":"mutate_sql","arguments":{{"source":"a","sql":"INSERT INTO users (id, name) VALUES ($1, $2)","params":[99,"mcp"],"approval_id":{approval_id}}}}}}}"#
    );
    let (status, _, body) = send(
        router.clone(),
        request(Method::POST, "/mcp", &retry, Some(&token), Some(&session)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["structuredContent"]["rowCount"], 1);
    assert_eq!(mock.mutations.load(Ordering::SeqCst), 1);

    let (status, _, body) = send(
        router,
        request(Method::POST, "/mcp", &retry, Some(&token), Some(&session)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_NOT_PENDING"
    );
    assert_eq!(mock.mutations.load(Ordering::SeqCst), 1);
}

fn elicitation_state(
    outcome: ElicitationOutcome,
) -> (TempDir, AppState, String, Arc<MockSqlExecutor>) {
    let (dir, mut state, token, _metadata) = setup();
    let mock = Arc::new(MockSqlExecutor::default());
    state.executor = mock.clone();
    state.elicitation = Arc::new(FixedElicitation(outcome));
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
        let profile = store
            .create_profile(&NewProfile {
                source_id: source.id.clone(),
                name: "mutate".to_string(),
                purpose: ProfilePurpose::Mutate,
                credential_ref: "keyring:mcp-db/test/query".to_string(),
                max_connections: Some(1),
                statement_timeout_ms: Some(1_000),
            })
            .expect("create profile");
        store
            .create_route(&NewRoute {
                source_id: source.id.clone(),
                tool: Tool::MutateSql,
                profile_id: profile.id,
                min_approval_policy: ApprovalPolicy::Direct,
            })
            .expect("create route");
        let token_id = store.authenticate(&token).expect("auth").token_id;
        store
            .create_grant(&NewGrant {
                token_id,
                source_id: source.id,
                tool: Tool::MutateSql,
                policy: Some(ApprovalPolicy::Approval),
            })
            .expect("create grant");
    }
    (dir, state, token, mock)
}

#[tokio::test]
async fn elicitation_accept_executes_once() {
    let (_dir, state, token, mock) = elicitation_state(ElicitationOutcome::Accepted);
    let router = app(state);
    let session = init_session(router.clone(), &token).await;
    let (status, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            MUTATE_INSERT,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["structuredContent"]["rowCount"], 1);
    assert_eq!(mock.mutations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn elicitation_decline_and_cancel_do_not_execute() {
    for outcome in [ElicitationOutcome::Declined, ElicitationOutcome::Cancelled] {
        let (_dir, state, token, mock) = elicitation_state(outcome);
        let router = app(state);
        let session = init_session(router.clone(), &token).await;
        let (status, _, body) = send(
            router,
            request(
                Method::POST,
                "/mcp",
                MUTATE_INSERT,
                Some(&token),
                Some(&session),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["result"]["structuredContent"]["code"],
            "APPROVAL_DENIED"
        );
        assert_eq!(mock.mutations.load(Ordering::SeqCst), 0);
    }
}

fn add_provision_access(state: &AppState, token: &str) {
    let store = state.store.lock().expect("store lock");
    let profile = store
        .create_profile(&NewProfile {
            source_id: "a".to_string(),
            name: "provision".to_string(),
            purpose: ProfilePurpose::Provision,
            credential_ref: "keyring:provision/test".to_string(),
            max_connections: None,
            statement_timeout_ms: None,
        })
        .expect("create provision profile");
    store
        .create_route(&NewRoute {
            source_id: "a".to_string(),
            tool: Tool::CreateDatabase,
            profile_id: profile.id,
            min_approval_policy: ApprovalPolicy::Approval,
        })
        .expect("create provision route");
    let token_id = store.authenticate(token).expect("auth").token_id;
    store
        .create_grant(&NewGrant {
            token_id,
            source_id: "a".to_string(),
            tool: Tool::CreateDatabase,
            policy: None,
        })
        .expect("create create_database grant");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

const CREATE_DATABASE: &str = r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"create_database","arguments":{"source":"a"}}}"#;

fn create_database_retry(approval_id: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{{"name":"create_database","arguments":{{"source":"a","approval_id":{approval_id}}}}}}}"#
    )
}

async fn request_creation(
    router: axum::Router,
    token: &str,
    session: &str,
) -> (Value, i64, String) {
    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            CREATE_DATABASE,
            Some(token),
            Some(session),
        ),
    )
    .await;
    let approval_id = body["result"]["structuredContent"]["approvalId"]
        .as_i64()
        .expect("approval id");
    let database = body["result"]["structuredContent"]["database"]
        .as_str()
        .expect("database name")
        .to_string();
    (body, approval_id, database)
}

#[tokio::test]
async fn create_database_requires_grant_and_an_enabled_provision_route() {
    let (_dir, state, token, _) = setup();
    add_metadata_access(&state, &token);
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            CREATE_DATABASE,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "TOOL_NOT_GRANTED"
    );
    assert!(state
        .store
        .lock()
        .unwrap()
        .list_pending_approvals(10)
        .unwrap()
        .is_empty());

    add_provision_access(&state, &token);
    let (body, _, database) = request_creation(router, &token, &session).await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_REQUIRED"
    );
    assert!(database.starts_with("mcpdb_np_"));
    assert_eq!(
        body["result"]["structuredContent"]["environment"],
        "nonproduction"
    );
    let pending = state
        .store
        .lock()
        .unwrap()
        .list_pending_approvals(10)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].channel, mcp_db::ApprovalChannel::Admin);
    assert!(pending[0].draft.high_risk);
    assert!(pending[0].draft.sql.is_none());
    let spec = mcp_db::ProvisionSpec::from_json(&pending[0].draft.params_json).expect("spec");
    assert_eq!(spec.database, database);
    assert!(spec
        .result_tools
        .iter()
        .all(|tool| tool != "create_database" && tool != "list_sources"));
}

#[tokio::test]
async fn approved_creation_registers_one_source_with_result_grants_only() {
    let (_dir, mut state, token, _) = setup();
    add_metadata_access(&state, &token);
    add_provision_access(&state, &token);
    let provisioner = Arc::new(MockProvisioner::new(ProvisionMode::Success));
    state.provisioner = provisioner.clone();
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (_, approval_id, database) = request_creation(router.clone(), &token, &session).await;
    assert_eq!(provisioner.calls.load(Ordering::SeqCst), 0);

    let approved = state
        .store
        .lock()
        .unwrap()
        .decide_approval(approval_id, true, "admin", now_ms(), 60_000)
        .unwrap();
    assert_eq!(approved, mcp_db::DecisionOutcome::Approved);

    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            &create_database_retry(approval_id),
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(body["result"]["structuredContent"]["source"], database);
    assert_eq!(
        body["result"]["structuredContent"]["approvalId"],
        approval_id
    );
    assert_eq!(provisioner.calls.load(Ordering::SeqCst), 1);

    {
        let store = state.store.lock().unwrap();
        let token_id = store.authenticate(&token).unwrap().token_id;
        let visible = store.list_visible_sources(&token_id).unwrap();
        assert!(visible.iter().any(|source| source.id == database));
        let grants = store.list_grants_for_token(&token_id).unwrap();
        let created: Vec<_> = grants
            .iter()
            .filter(|grant| grant.source_id == database)
            .map(|grant| grant.tool)
            .collect();
        assert!(created.contains(&Tool::QuerySql));
        assert!(!created.contains(&Tool::CreateDatabase));
        let other = store.create_token("other-agent").unwrap();
        assert!(store
            .list_visible_sources(&other.token_id)
            .unwrap()
            .iter()
            .all(|source| source.id != database));
        assert_eq!(
            store.list_grants_for_token(&other.token_id).unwrap().len(),
            0
        );
        let source = store.get_source(&database).unwrap();
        assert_eq!(source.parent_source_id.as_deref(), Some("a"));
        assert_eq!(source.environment, Environment::NonProduction);
    }

    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            &create_database_retry(approval_id),
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_NOT_PENDING"
    );
    assert_eq!(provisioner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn quota_and_quarantine_block_before_an_approval_is_created() {
    let (_dir, mut state, token, _) = setup();
    add_metadata_access(&state, &token);
    add_provision_access(&state, &token);
    state.provision_limits = ProvisionLimits::new(1, 1, vec![Tool::QuerySql]).unwrap();
    let token_id = state
        .store
        .lock()
        .unwrap()
        .authenticate(&token)
        .unwrap()
        .token_id;
    let reservation = state
        .store
        .lock()
        .unwrap()
        .reserve_provision(
            &token_id,
            "a",
            "mcpdb_np_manual",
            None,
            &state.provision_limits,
            now_ms(),
        )
        .unwrap();
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            CREATE_DATABASE,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "RESOURCE_LIMIT"
    );
    assert!(state
        .store
        .lock()
        .unwrap()
        .list_pending_approvals(10)
        .unwrap()
        .is_empty());

    state
        .store
        .lock()
        .unwrap()
        .settle_reservation(
            reservation.id,
            mcp_db::ReservationStatus::Quarantined,
            Some("EXECUTION_UNKNOWN"),
            now_ms(),
        )
        .unwrap();
    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            CREATE_DATABASE,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "PROVISION_BLOCKED"
    );
}

#[tokio::test]
async fn unknown_provisioning_result_quarantines_and_blocks_retry() {
    let (_dir, mut state, token, _) = setup();
    add_metadata_access(&state, &token);
    add_provision_access(&state, &token);
    let provisioner = Arc::new(MockProvisioner::new(ProvisionMode::Unknown));
    state.provisioner = provisioner.clone();
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (_, approval_id, database) = request_creation(router.clone(), &token, &session).await;
    state
        .store
        .lock()
        .unwrap()
        .decide_approval(approval_id, true, "admin", now_ms(), 60_000)
        .unwrap();
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            &create_database_retry(approval_id),
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "EXECUTION_UNKNOWN"
    );
    {
        let store = state.store.lock().unwrap();
        let reservation = store
            .reservation_for_approval(approval_id)
            .unwrap()
            .expect("reservation");
        assert_eq!(reservation.status, mcp_db::ReservationStatus::Quarantined);
        assert!(store.get_source(&database).is_err());
    }
    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            CREATE_DATABASE,
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "PROVISION_BLOCKED"
    );
    assert_eq!(provisioner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn registration_failure_after_creation_quarantines_the_reservation() {
    let (_dir, mut state, token, _) = setup();
    add_metadata_access(&state, &token);
    add_provision_access(&state, &token);
    let provisioner = Arc::new(MockProvisioner::new(ProvisionMode::Success));
    state.provisioner = provisioner.clone();
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (_, approval_id, database) = request_creation(router.clone(), &token, &session).await;
    {
        // Occupy the generated database name locally so registration must fail.
        let store = state.store.lock().unwrap();
        store
            .create_source(&NewSource {
                id: database.clone(),
                driver: Driver::Postgres,
                environment: Environment::NonProduction,
                parent_source_id: None,
                created_token_id: None,
                created_agent: None,
                created_origin: None,
            })
            .unwrap();
        store
            .decide_approval(approval_id, true, "admin", now_ms(), 60_000)
            .unwrap();
    }
    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            &create_database_retry(approval_id),
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "PROVISION_REGISTRATION_FAILED"
    );
    let store = state.store.lock().unwrap();
    let reservation = store
        .reservation_for_approval(approval_id)
        .unwrap()
        .expect("reservation");
    assert_eq!(reservation.status, mcp_db::ReservationStatus::Quarantined);
    assert_eq!(provisioner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn not_started_provisioning_releases_the_reservation() {
    let (_dir, mut state, token, _) = setup();
    add_metadata_access(&state, &token);
    add_provision_access(&state, &token);
    state.provision_limits = ProvisionLimits::new(5, 1, vec![Tool::QuerySql]).unwrap();
    let provisioner = Arc::new(MockProvisioner::new(ProvisionMode::NotStarted));
    state.provisioner = provisioner.clone();
    let router = app(state.clone());
    let session = init_session(router.clone(), &token).await;
    let (_, approval_id, _) = request_creation(router.clone(), &token, &session).await;
    state
        .store
        .lock()
        .unwrap()
        .decide_approval(approval_id, true, "admin", now_ms(), 60_000)
        .unwrap();
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            &create_database_retry(approval_id),
            Some(&token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "EXECUTION_NOT_STARTED"
    );
    {
        let store = state.store.lock().unwrap();
        let reservation = store
            .reservation_for_approval(approval_id)
            .unwrap()
            .expect("reservation");
        assert_eq!(reservation.status, mcp_db::ReservationStatus::Released);
        let token_id = store.authenticate(&token).unwrap().token_id;
        assert_eq!(
            store
                .provision_usage(&token_id, "a", now_ms(), 1)
                .unwrap()
                .quarantined,
            0
        );
    }
    // A released reservation does not block the next request.
    let (body, _, _) = request_creation(router, &token, &session).await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_REQUIRED"
    );
    assert_eq!(provisioner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "requires MCPDB_TEST_PG_ADMIN_DSN with CREATEDB and CREATEROLE"]
async fn create_database_end_to_end_with_real_postgres() {
    let dsn = env::var("MCPDB_TEST_PG_ADMIN_DSN").expect("MCPDB_TEST_PG_ADMIN_DSN");
    let dir = TempDir::new().expect("temp dir");
    let store_path = dir.path().join("credentials.enc");
    let key_path = dir.path().join("master.key");
    let key = [5_u8; 32];
    fs::write(
        &key_path,
        key.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
    .expect("write key");
    let entries = BTreeMap::from([("keyring:provision/test".to_string(), dsn.clone())]);
    EncryptedFileResolver::seal(&store_path, &key, &entries).expect("seal");
    let resolver =
        Arc::new(EncryptedFileResolver::open(&store_path, &key_path).expect("open resolver"));

    let store = Store::open(dir.path().join("state.db")).expect("open store");
    store
        .create_source(&NewSource {
            id: "parent".to_string(),
            driver: Driver::Postgres,
            environment: Environment::NonProduction,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: None,
        })
        .unwrap();
    let provision = store
        .create_profile(&NewProfile {
            source_id: "parent".to_string(),
            name: "provision".to_string(),
            purpose: ProfilePurpose::Provision,
            credential_ref: "keyring:provision/test".to_string(),
            max_connections: None,
            statement_timeout_ms: None,
        })
        .unwrap();
    store
        .create_route(&NewRoute {
            source_id: "parent".to_string(),
            tool: Tool::CreateDatabase,
            profile_id: provision.id,
            min_approval_policy: ApprovalPolicy::Approval,
        })
        .unwrap();
    let issued = store.create_token("e2e").expect("token");
    store
        .create_grant(&NewGrant {
            token_id: issued.token_id.clone(),
            source_id: "parent".to_string(),
            tool: Tool::CreateDatabase,
            policy: None,
        })
        .unwrap();

    let connections = Arc::new(mcp_db::postgres::ProfileConnections::default());
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        metadata: Arc::new(PostgresMetadata::with_connections(
            resolver.clone(),
            connections.clone(),
        )),
        executor: Arc::new(PostgresExecutor::with_connections(
            resolver.clone(),
            connections,
        )),
        provisioner: Arc::new(mcp_db::PostgresProvisioner::new(resolver.clone(), resolver)),
        provision_limits: ProvisionLimits::default(),
        rate_limiter: Arc::new(TokenRateLimiter::default()),
        elicitation: Arc::new(PeerElicitation),
        max_rows: DEFAULT_MAX_ROWS,
        approval_timeout_ms: 120_000,
        approval_execution_ttl_ms: 60_000,
        sessions: Default::default(),
    };
    let router = app(state.clone());
    let session = init_session(router.clone(), &issued.token).await;
    let create = r#"{"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"create_database","arguments":{"source":"parent"}}}"#;
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            create,
            Some(&issued.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "APPROVAL_REQUIRED"
    );
    let approval_id = body["result"]["structuredContent"]["approvalId"]
        .as_i64()
        .expect("approval id");
    let database = body["result"]["structuredContent"]["database"]
        .as_str()
        .expect("database name")
        .to_string();
    state
        .store
        .lock()
        .unwrap()
        .decide_approval(approval_id, true, "admin", now_ms(), 60_000)
        .unwrap();
    let retry = format!(
        r#"{{"jsonrpc":"2.0","id":32,"method":"tools/call","params":{{"name":"create_database","arguments":{{"source":"parent","approval_id":{approval_id}}}}}}}"#
    );
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            &retry,
            Some(&issued.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], false, "{body}");
    assert_eq!(body["result"]["structuredContent"]["source"], database);

    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&issued.token),
            Some(&session),
        ),
    )
    .await;
    assert!(body["result"]["structuredContent"]["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .any(|source| source["id"] == database));

    let query = format!(
        r#"{{"jsonrpc":"2.0","id":33,"method":"tools/call","params":{{"name":"query_sql","arguments":{{"source":"{database}","sql":"SELECT 1 AS value"}}}}}}"#
    );
    let (_, _, body) = send(
        router.clone(),
        request(
            Method::POST,
            "/mcp",
            &query,
            Some(&issued.token),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], false, "{body}");
    assert_eq!(body["result"]["structuredContent"]["rowCount"], 1);

    let other = state.store.lock().unwrap().create_token("other").unwrap();
    let other_session = init_session(router.clone(), &other.token).await;
    let (_, _, body) = send(
        router,
        request(
            Method::POST,
            "/mcp",
            LIST_SOURCES,
            Some(&other.token),
            Some(&other_session),
        ),
    )
    .await;
    assert!(body["result"]["structuredContent"]["sources"]
        .as_array()
        .expect("sources")
        .is_empty());

    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .expect("cleanup connection");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // DROP DATABASE cannot run inside the implicit transaction of a multi-statement batch.
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS \"{database}\""))
        .await
        .expect("cleanup database");
    for role in [format!("{database}_ro"), format!("{database}_rw")] {
        client
            .batch_execute(&format!("DROP ROLE IF EXISTS \"{role}\""))
            .await
            .expect("cleanup role");
    }
}
