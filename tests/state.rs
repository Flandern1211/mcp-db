use mcp_db::auth::hash_token;
use mcp_db::{
    ApprovalPolicy, Driver, Environment, Error, NewGrant, NewProfile, NewRoute, NewSource,
    ProfilePurpose, Store, Tool,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    (dir, store)
}

fn source(id: &str, environment: Environment) -> NewSource {
    NewSource {
        id: id.to_string(),
        driver: Driver::Postgres,
        environment,
        parent_source_id: None,
        created_token_id: None,
        created_agent: None,
        created_origin: None,
    }
}

fn profile(source_id: &str, name: &str, purpose: ProfilePurpose) -> NewProfile {
    NewProfile {
        source_id: source_id.to_string(),
        name: name.to_string(),
        purpose,
        credential_ref: "keyring:mcp-db/test/query".to_string(),
        max_connections: Some(5),
        statement_timeout_ms: Some(1_000),
    }
}

#[test]
fn token_is_hashed_and_revocable_across_restart() {
    let (dir, store) = store();
    let issued = store.create_token("agent-a").expect("issue token");
    assert!(issued.token.starts_with("mcpdb_"));
    assert_eq!(
        store.authenticate(&issued.token).unwrap().token_id,
        issued.token_id
    );

    store.revoke_token(&issued.token_id).expect("revoke");
    assert!(matches!(
        store.authenticate(&issued.token),
        Err(Error::TokenRevoked)
    ));
    let path = dir.path().join("state.db");
    drop(store);

    let connection = Connection::open(&path).expect("inspect state");
    let stored_hash: String = connection
        .query_row(
            "SELECT token_hash FROM tokens WHERE id = ?",
            [&issued.token_id],
            |row| row.get(0),
        )
        .expect("read token hash");
    assert_ne!(stored_hash, issued.token);
    assert_eq!(stored_hash, hash_token(&issued.token));
    drop(connection);

    let store = Store::open(&path).expect("reopen store");
    assert!(matches!(
        store.authenticate(&issued.token),
        Err(Error::TokenRevoked)
    ));
}

#[test]
fn unknown_token_is_rejected() {
    let (_dir, store) = store();
    assert!(matches!(
        store.authenticate("mcpdb_not-a-real-token"),
        Err(Error::Unauthenticated)
    ));
}

#[test]
fn models_persist_across_restart() {
    let (dir, store) = store();
    let issued = store.create_token("agent-a").expect("issue token");
    let created_source = store
        .create_source(&source("main", Environment::NonProduction))
        .expect("create source");
    let query_primary = store
        .create_profile(&profile("main", "query-primary", ProfilePurpose::Query))
        .expect("create query profile");
    store
        .create_profile(&profile("main", "query-secondary", ProfilePurpose::Query))
        .expect("create second query profile");
    let mutate = store
        .create_profile(&profile("main", "mutate", ProfilePurpose::Mutate))
        .expect("create mutate profile");
    let route = store
        .create_route(&NewRoute {
            source_id: "main".to_string(),
            tool: Tool::QuerySql,
            profile_id: query_primary.id,
            min_approval_policy: ApprovalPolicy::Direct,
        })
        .expect("create route");
    store
        .create_grant(&NewGrant {
            token_id: issued.token_id.clone(),
            source_id: "main".to_string(),
            tool: Tool::QuerySql,
            policy: None,
        })
        .expect("create read grant");
    store
        .create_grant(&NewGrant {
            token_id: issued.token_id.clone(),
            source_id: "main".to_string(),
            tool: Tool::MutateSql,
            policy: None,
        })
        .expect("create write grant");

    assert_eq!(created_source.status, mcp_db::SourceStatus::Active);
    assert_eq!(store.list_profiles("main").unwrap().len(), 3);
    assert_eq!(route.profile_id, query_primary.id);
    assert_eq!(mutate.purpose, ProfilePurpose::Mutate);
    assert_eq!(
        store.list_grants_for_token(&issued.token_id).unwrap().len(),
        2
    );

    let path = dir.path().join("state.db");
    drop(store);
    let store = Store::open(&path).expect("reopen store");
    assert_eq!(store.get_source("main").unwrap().id, "main");
    assert_eq!(store.list_profiles("main").unwrap().len(), 3);
    assert_eq!(
        store.get_route("main", Tool::QuerySql).unwrap().id,
        route.id
    );
    assert_eq!(
        store.list_grants_for_token(&issued.token_id).unwrap().len(),
        2
    );
    assert_eq!(store.schema_version().unwrap(), 1);
}

#[test]
fn production_sources_accept_only_read_grants() {
    let (_dir, store) = store();
    let issued = store.create_token("agent-a").expect("issue token");
    store
        .create_source(&source("prod", Environment::Production))
        .expect("create production source");

    let read = store.create_grant(&NewGrant {
        token_id: issued.token_id.clone(),
        source_id: "prod".to_string(),
        tool: Tool::QuerySql,
        policy: None,
    });
    assert!(read.is_ok());

    let write = store.create_grant(&NewGrant {
        token_id: issued.token_id,
        source_id: "prod".to_string(),
        tool: Tool::MutateSql,
        policy: Some(ApprovalPolicy::Approval),
    });
    assert!(matches!(write, Err(Error::InvalidInput(_))));
}

#[test]
fn route_requires_matching_profile_purpose() {
    let (_dir, store) = store();
    store
        .create_source(&source("main", Environment::NonProduction))
        .expect("create source");
    let mutate = store
        .create_profile(&profile("main", "mutate", ProfilePurpose::Mutate))
        .expect("create mutate profile");

    let mismatch = store.create_route(&NewRoute {
        source_id: "main".to_string(),
        tool: Tool::QuerySql,
        profile_id: mutate.id,
        min_approval_policy: ApprovalPolicy::Direct,
    });
    assert!(matches!(mismatch, Err(Error::InvalidInput(_))));

    let discovery = store.create_route(&NewRoute {
        source_id: "main".to_string(),
        tool: Tool::ListSources,
        profile_id: mutate.id,
        min_approval_policy: ApprovalPolicy::Direct,
    });
    assert!(matches!(discovery, Err(Error::InvalidInput(_))));
}

#[test]
fn credential_refs_must_not_contain_dsns_or_passwords() {
    let (_dir, store) = store();
    store
        .create_source(&source("main", Environment::NonProduction))
        .expect("create source");

    let plaintext = store.create_profile(&NewProfile {
        credential_ref: "postgres://user:password@db.example/app".to_string(),
        ..profile("main", "plaintext", ProfilePurpose::Query)
    });
    assert!(matches!(plaintext, Err(Error::InvalidInput(_))));

    let keyring = store.create_profile(&profile("main", "keyring", ProfilePurpose::Query));
    assert!(keyring.is_ok());
}

#[test]
fn revoked_tokens_cannot_receive_new_grants() {
    let (_dir, store) = store();
    let issued = store.create_token("agent-a").expect("issue token");
    store.revoke_token(&issued.token_id).expect("revoke");
    store
        .create_source(&source("main", Environment::NonProduction))
        .expect("create source");

    let grant = store.create_grant(&NewGrant {
        token_id: issued.token_id,
        source_id: "main".to_string(),
        tool: Tool::QuerySql,
        policy: None,
    });
    assert!(matches!(grant, Err(Error::TokenRevoked)));
}

#[test]
fn legacy_approvals_and_audit_survive_upgrade() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("state.db");
    let connection = Connection::open(&path).expect("legacy state");
    connection
        .execute_batch(
            "CREATE TABLE approvals (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               agent TEXT NOT NULL,
               source TEXT NOT NULL,
               tool TEXT NOT NULL,
               sql TEXT NOT NULL,
               params_json TEXT,
               status TEXT NOT NULL,
               channel TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               expires_at INTEGER NOT NULL,
               decided_at INTEGER
             );
             CREATE TABLE audit (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               ts TEXT NOT NULL,
               agent TEXT NOT NULL,
               source TEXT,
               tool TEXT NOT NULL,
               sql TEXT,
               params_json TEXT,
               duration_ms INTEGER,
               row_count INTEGER,
               decision TEXT NOT NULL,
               channel TEXT,
               reason TEXT
             );
             INSERT INTO approvals
               (agent, source, tool, sql, params_json, status, channel, created_at, expires_at, decided_at)
               VALUES ('legacy', 'old', 'execute_sql', 'SELECT 1', '[]', 'approved', 'cli', 1, 2, 3);
             INSERT INTO audit
               (ts, agent, source, tool, sql, params_json, duration_ms, row_count, decision, channel, reason)
               VALUES ('2026-11-11T00:00:00Z', 'legacy', 'old', 'execute_sql', 'SELECT 1', '[]', 1, 1, 'allowed', NULL, NULL);",
        )
        .expect("legacy fixture");
    drop(connection);

    let store = Store::open(&path).expect("upgrade store");
    assert_eq!(store.schema_version().unwrap(), 1);
    assert!(store.authenticate("legacy-token").is_err());
    drop(store);

    let connection = Connection::open(&path).expect("inspect upgraded state");
    let approvals: i64 = connection
        .query_row("SELECT COUNT(*) FROM approvals", [], |row| row.get(0))
        .expect("count approvals");
    let audit: i64 = connection
        .query_row("SELECT COUNT(*) FROM audit", [], |row| row.get(0))
        .expect("count audit");
    let tokens: i64 = connection
        .query_row("SELECT COUNT(*) FROM tokens", [], |row| row.get(0))
        .expect("count tokens");
    assert_eq!(approvals, 1);
    assert_eq!(audit, 1);
    assert_eq!(tokens, 0);
}
