use mcp_db::auth::hash_token;
use mcp_db::{
    ApprovalPolicy, AuditEvent, Authorizer, Driver, Environment, Error, NewGrant, NewProfile,
    NewRoute, NewSource, ProfilePurpose, Store, Tool,
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
fn unsupported_limits_are_rejected_before_persistence() {
    let (_dir, store) = store();
    store
        .create_source(&source("limits", Environment::NonProduction))
        .unwrap();
    for (connections, timeout) in [
        (Some(0), None),
        (Some(1001), None),
        (None, Some(0)),
        (None, Some(u64::MAX)),
    ] {
        let mut input = profile("limits", "invalid", ProfilePurpose::Query);
        input.max_connections = connections;
        input.statement_timeout_ms = timeout;
        assert!(matches!(
            store.create_profile(&input),
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(store.list_profiles("limits").unwrap().is_empty());
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
    assert_eq!(store.schema_version().unwrap(), 6);
}

#[test]
fn admin_keys_never_expire_until_revoked() {
    let (_dir, store) = store();
    let now = 1_000;
    let key = store.create_admin_key("laptop", now).unwrap();
    assert!(key.starts_with("mcpdb_adminkey_"));
    store
        .authenticate_admin_key(&key, now + 10_000_000)
        .unwrap();
    let listed = store.list_admin_keys().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "laptop");
    assert!(listed[0].last_used_at.is_some());
    store
        .revoke_admin_key(listed[0].id, now + 20_000_000)
        .unwrap();
    assert!(store
        .authenticate_admin_key(&key, now + 30_000_000)
        .is_err());
    assert!(store.create_admin_key("  ", now).is_err());
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

    let provision = store
        .create_profile(&NewProfile {
            credential_ref: "keyring:provision/test".to_string(),
            ..profile("main", "provision", ProfilePurpose::Provision)
        })
        .expect("create provision profile");
    let query_on_provision = store.create_route(&NewRoute {
        source_id: "main".to_string(),
        tool: Tool::QuerySql,
        profile_id: provision.id,
        min_approval_policy: ApprovalPolicy::Direct,
    });
    assert!(matches!(query_on_provision, Err(Error::InvalidInput(_))));
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

    // The provision admin identity stays out of ordinary profile namespaces.
    let ordinary_with_admin_ref = store.create_profile(&NewProfile {
        credential_ref: "keyring:provision/test".to_string(),
        ..profile("main", "leak", ProfilePurpose::Query)
    });
    assert!(matches!(
        ordinary_with_admin_ref,
        Err(Error::InvalidInput(_))
    ));
    let provision_without_admin_ref =
        store.create_profile(&profile("main", "provision", ProfilePurpose::Provision));
    assert!(matches!(
        provision_without_admin_ref,
        Err(Error::InvalidInput(_))
    ));
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
    assert_eq!(store.schema_version().unwrap(), 6);
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
    let grants: i64 = connection
        .query_row("SELECT COUNT(*) FROM tool_grants", [], |row| row.get(0))
        .expect("count grants");
    assert_eq!(approvals, 1);
    assert_eq!(audit, 1);
    assert_eq!(tokens, 0);
    assert_eq!(grants, 0);
}

fn audit_event(ts: i64, event: &str) -> AuditEvent {
    AuditEvent {
        ts,
        event: event.to_string(),
        approval_id: None,
        token_id: None,
        agent: None,
        source_id: Some("a".to_string()),
        tool: Tool::ListTables.as_str().to_string(),
        profile_id: None,
        profile_version: None,
        route_id: None,
        route_version: None,
        request_digest: None,
        channel: None,
        decision: "allowed".to_string(),
        actor: "admin-cli".to_string(),
        high_risk: false,
        reason_code: None,
        reason: None,
        duration_ms: None,
        row_count: None,
        error_code: None,
    }
}

#[test]
fn admin_login_tokens_store_only_a_hash_and_are_single_use() {
    let (dir, store) = store();
    let raw = store
        .create_admin_login_token(1_000, 300_000)
        .expect("login token");
    assert!(raw.starts_with("mcpdb_admin_"));

    let connection = Connection::open(dir.path().join("state.db")).expect("inspect state");
    let stored: String = connection
        .query_row("SELECT token_hash FROM admin_login_tokens", [], |row| {
            row.get(0)
        })
        .expect("stored hash");
    assert_eq!(stored, hash_token(&raw));
    assert_ne!(stored, raw);
    let mut statement = connection
        .prepare("PRAGMA table_info(admin_login_tokens)")
        .expect("table info");
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("columns")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("column names");
    assert_eq!(
        columns,
        vec!["id", "token_hash", "created_at", "expires_at", "used_at"]
    );
    drop(statement);
    drop(connection);

    store
        .consume_admin_login_token(&raw, 1_000)
        .expect("first use");
    assert!(matches!(
        store.consume_admin_login_token(&raw, 1_000),
        Err(Error::Unauthenticated)
    ));

    let expired = store
        .create_admin_login_token(1_000, 10)
        .expect("login token");
    assert!(store.consume_admin_login_token(&expired, 2_000).is_err());
    assert!(store
        .consume_admin_login_token("mcpdb_admin_unknown", 1_000)
        .is_err());

    // Issuing a token drops expired rows so the table cannot grow without bound.
    store
        .create_admin_login_token(2_000, 300_000)
        .expect("login token");
    let connection = Connection::open(dir.path().join("state.db")).expect("inspect state");
    let expired_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM admin_login_tokens WHERE expires_at <= 2000",
            [],
            |row| row.get(0),
        )
        .expect("expired count");
    let total: i64 = connection
        .query_row("SELECT COUNT(*) FROM admin_login_tokens", [], |row| {
            row.get(0)
        })
        .expect("total count");
    assert_eq!(expired_rows, 0);
    assert_eq!(total, 2);
}

#[test]
fn revoked_grant_no_longer_authorizes_or_is_listed() {
    let (_dir, store) = store();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    let profile = store
        .create_profile(&profile("a", "query", ProfilePurpose::Query))
        .unwrap();
    store
        .create_route(&NewRoute {
            source_id: "a".to_string(),
            tool: Tool::ListTables,
            profile_id: profile.id,
            min_approval_policy: ApprovalPolicy::Direct,
        })
        .unwrap();
    let token = store.create_token("agent-a").unwrap();
    store
        .create_grant(&NewGrant {
            token_id: token.token_id.clone(),
            source_id: "a".to_string(),
            tool: Tool::ListTables,
            policy: None,
        })
        .unwrap();
    assert!(Authorizer::new(&store)
        .authorize(&token.token_id, Some("a"), Tool::ListTables)
        .is_ok());

    store
        .revoke_grant(&token.token_id, "a", Tool::ListTables, "admin-cli")
        .expect("revoke");
    assert!(matches!(
        Authorizer::new(&store).authorize(&token.token_id, Some("a"), Tool::ListTables),
        Err(Error::ToolNotGranted(_))
    ));
    assert!(store
        .list_grants_for_token(&token.token_id)
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.revoke_grant(&token.token_id, "a", Tool::ListTables, "admin-cli"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn recent_audit_is_newest_first_with_stable_ties() {
    let (_dir, store) = store();
    for (ts, event) in [(100, "first"), (300, "second"), (300, "third")] {
        store.record_audit_event(&audit_event(ts, event)).unwrap();
    }
    let events = store.list_recent_audit(10).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event.as_str())
            .collect::<Vec<_>>(),
        vec!["third", "second", "first"]
    );
    let limited = store.list_recent_audit(1).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].event, "third");
}
