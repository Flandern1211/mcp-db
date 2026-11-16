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
    PeerElicitation, PostgresExecutor, PostgresMetadata, Profile, ProfilePurpose, QueryOutcome,
    SqlExecutor, Store, Tool, UnavailableExecutor, DEFAULT_MAX_ROWS,
};
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

#[derive(Default)]
struct MockSqlExecutor {
    mutations: AtomicUsize,
}

struct FixedElicitation(ElicitationOutcome);

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

fn setup() -> (TempDir, AppState, String, Arc<MockMetadata>) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let token = store.create_token("agent-a").expect("issue token");
    let metadata = Arc::new(MockMetadata::default());
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        metadata: metadata.clone(),
        executor: Arc::new(UnavailableExecutor),
        elicitation: Arc::new(PeerElicitation),
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
    serde_json::from_str(text.trim()).expect("JSON or SSE body")
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
        elicitation: Arc::new(PeerElicitation),
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
        elicitation: Arc::new(PeerElicitation),
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
