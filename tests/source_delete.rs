use mcp_db::{
    ApprovalChannel, ApprovalPolicy, ApprovalStatus, Driver, Environment, Error,
    NewApprovalRequest, NewGrant, NewProfile, NewRoute, NewSource, ProfilePurpose, ProvisionLimits,
    ReservationStatus, SourceStatus, Store, Tool,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    (dir, store)
}

fn store_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("state.db")
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

fn child(id: &str, parent: &str) -> NewSource {
    NewSource {
        parent_source_id: Some(parent.to_string()),
        ..source(id, Environment::NonProduction)
    }
}

fn limits(max_active: u32, min_interval_ms: i64) -> ProvisionLimits {
    ProvisionLimits::new(max_active, min_interval_ms, vec![Tool::QuerySql]).unwrap()
}

fn setup() -> (TempDir, Store, String) {
    let (dir, store) = store();
    store
        .create_source(&source("parent", Environment::NonProduction))
        .unwrap();
    let token = store.create_token("agent").unwrap().token_id;
    (dir, store, token)
}

fn add_profile(store: &Store, source_id: &str, name: &str) -> mcp_db::Profile {
    store
        .create_profile(&NewProfile {
            source_id: source_id.to_string(),
            name: name.to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: format!("keyring:mcp-db/test/{source_id}/{name}"),
            max_connections: None,
            statement_timeout_ms: None,
        })
        .expect("create profile")
}

fn add_route(store: &Store, source_id: &str, tool: Tool, profile_id: i64) -> mcp_db::Route {
    store
        .create_route(&NewRoute {
            source_id: source_id.to_string(),
            tool,
            profile_id,
            min_approval_policy: ApprovalPolicy::Direct,
        })
        .expect("create route")
}

fn pending_approval(source: &str, token: &str) -> NewApprovalRequest {
    NewApprovalRequest {
        draft: mcp_db::ApprovalDraft {
            token_id: token.to_string(),
            agent: "agent".to_string(),
            source_id: source.to_string(),
            source_environment: Environment::NonProduction,
            tool: Tool::MutateSql,
            sql: Some("UPDATE items SET name = 'x'".to_string()),
            params_json: "[]".to_string(),
            profile_id: 1,
            profile_version: 1,
            route_id: 1,
            route_version: 1,
            grant_policy: ApprovalPolicy::Approval,
            route_min_policy: ApprovalPolicy::Approval,
            high_risk: false,
        },
        channel: ApprovalChannel::Admin,
        status: ApprovalStatus::Pending,
        created_at: 9_000_000,
        expires_at: 9_120_000,
        decided_at: None,
        decided_by: None,
        reason_code: None,
        reason: None,
    }
}

#[test]
fn delete_cascades_control_plane_rows_and_frees_the_id() {
    let (dir, store, token) = setup();
    let created = store
        .create_source(&child("mcpdb_np_leaf", "parent"))
        .unwrap();
    let profile = add_profile(&store, "mcpdb_np_leaf", "query_ro");
    add_route(&store, "mcpdb_np_leaf", Tool::QuerySql, profile.id);
    store
        .create_grant(&NewGrant {
            token_id: token.clone(),
            source_id: "mcpdb_np_leaf".to_string(),
            tool: Tool::QuerySql,
            policy: Some(ApprovalPolicy::Direct),
        })
        .unwrap();

    let deleted = store
        .delete_source("mcpdb_np_leaf", "admin", 9_500_000)
        .unwrap();
    assert_eq!(deleted.id, created.id);
    assert_eq!(deleted.driver, Driver::Postgres);
    assert_eq!(deleted.environment, Environment::NonProduction);

    assert!(matches!(
        store.get_source("mcpdb_np_leaf"),
        Err(Error::NotFound(_))
    ));
    assert!(store.list_profiles("mcpdb_np_leaf").unwrap().is_empty());
    assert!(store.list_routes("mcpdb_np_leaf").unwrap().is_empty());
    assert!(store
        .list_grants_for_token(&token)
        .unwrap()
        .iter()
        .all(|grant| grant.source_id != "mcpdb_np_leaf"));
    assert!(store
        .list_all_sources()
        .unwrap()
        .iter()
        .all(|source| source.id != "mcpdb_np_leaf"));

    // The freed id can be created again.
    store
        .create_source(&child("mcpdb_np_leaf", "parent"))
        .unwrap();
    assert_eq!(
        store.get_source("mcpdb_np_leaf").unwrap().status,
        SourceStatus::Active
    );
    let _dir = dir;
}

#[test]
fn delete_records_a_allowed_source_deleted_audit() {
    let (_dir, store, _token) = setup();
    store
        .create_source(&child("mcpdb_np_audited", "parent"))
        .unwrap();
    store
        .delete_source("mcpdb_np_audited", "admin", 9_500_100)
        .unwrap();

    let event = store
        .list_recent_audit(10)
        .unwrap()
        .into_iter()
        .find(|event| event.event == "deleted")
        .expect("SOURCE_DELETED audit event");
    assert_eq!(event.reason_code.as_deref(), Some("SOURCE_DELETED"));
    assert_eq!(event.decision, "allowed");
    assert_eq!(event.source_id.as_deref(), Some("mcpdb_np_audited"));
    assert_eq!(event.actor, "admin");
}

#[test]
fn pending_or_approved_approvals_block_deletion() {
    let (_dir, store, token) = setup();
    store
        .create_source(&child("mcpdb_np_guard", "parent"))
        .unwrap();
    store
        .insert_approval_request(&pending_approval("mcpdb_np_guard", &token))
        .unwrap();

    let before_profiles = store.list_profiles("mcpdb_np_guard").unwrap().len();
    let before_routes = store.list_routes("mcpdb_np_guard").unwrap().len();
    assert!(matches!(
        store.delete_source("mcpdb_np_guard", "admin", 9_500_200),
        Err(Error::Conflict(message)) if message.contains("待批或已批准请求")
    ));
    store.get_source("mcpdb_np_guard").unwrap();
    assert_eq!(
        store.list_profiles("mcpdb_np_guard").unwrap().len(),
        before_profiles
    );
    assert_eq!(
        store.list_routes("mcpdb_np_guard").unwrap().len(),
        before_routes
    );
}

#[test]
fn child_sources_block_deletion() {
    let (_dir, store, _token) = setup();
    store
        .create_source(&child("mcpdb_np_child", "parent"))
        .unwrap();
    assert!(matches!(
        store.delete_source("parent", "admin", 9_500_300),
        Err(Error::Conflict(message)) if message.contains("子数据源")
    ));
    store.get_source("parent").unwrap();
    store.get_source("mcpdb_np_child").unwrap();
}

#[test]
fn unsettled_reservations_block_deletion() {
    let (_dir, store, token) = setup();
    store
        .reserve_provision(
            &token,
            "parent",
            "mcpdb_np_unsettled",
            None,
            &limits(5, 1),
            9_500_400,
        )
        .unwrap();
    assert!(matches!(
        store.delete_source("parent", "admin", 9_500_401),
        Err(Error::Conflict(message)) if message.contains("未结算的建库预留")
    ));
    store.get_source("parent").unwrap();
    assert!(store
        .reservation_by_database("mcpdb_np_unsettled")
        .unwrap()
        .is_some());
}

#[test]
fn settled_reservations_are_removed_with_the_parent() {
    let (_dir, store, token) = setup();
    let reservation = store
        .reserve_provision(
            &token,
            "parent",
            "mcpdb_np_settled",
            None,
            &limits(5, 1),
            9_500_500,
        )
        .unwrap();
    store
        .create_source(&child("mcpdb_np_settled", "parent"))
        .unwrap();
    store
        .settle_reservation(
            reservation.id,
            ReservationStatus::Committed,
            None,
            9_500_501,
        )
        .unwrap();

    store
        .delete_source("mcpdb_np_settled", "admin", 9_500_502)
        .unwrap();
    assert!(store
        .reservation_by_database("mcpdb_np_settled")
        .unwrap()
        .is_none());
    // Deleting the provisioned child frees the parent, whose own deletion then
    // removes any remaining reservation rows pointing at it.
    store.delete_source("parent", "admin", 9_500_503).unwrap();
    // The parent is gone, so this token can no longer build against it.
    assert!(matches!(
        store.get_source("parent"),
        Err(Error::NotFound(_))
    ));
    let connection = Connection::open(store_path(_dir.path())).unwrap();
    let rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM quota_reservations
              WHERE parent_source_id = 'parent' OR database_name = 'parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        rows, 0,
        "settled reservation rows must be removed with the parent"
    );
}

#[test]
fn quarantined_reservations_block_deletion_of_both_parent_and_database_name() {
    let (_dir, store, token) = setup();
    let reservation = store
        .reserve_provision(
            &token,
            "parent",
            "mcpdb_np_quarantined",
            None,
            &limits(5, 1),
            9_500_600,
        )
        .unwrap();
    store
        .settle_reservation(
            reservation.id,
            ReservationStatus::Quarantined,
            Some("EXECUTION_UNKNOWN"),
            9_500_601,
        )
        .unwrap();
    // Quarantined by parent_source_id…
    assert!(matches!(
        store.delete_source("parent", "admin", 9_500_602),
        Err(Error::Conflict(message)) if message.contains("未结算的建库预留")
    ));
    // …and a quarantined reservation whose database_name matches a still existing source.
    store
        .create_source(&child("mcpdb_np_quarantined", "parent"))
        .unwrap();
    assert!(matches!(
        store.delete_source("mcpdb_np_quarantined", "admin", 9_500_603),
        Err(Error::Conflict(message)) if message.contains("未结算的建库预留")
    ));
    store.get_source("mcpdb_np_quarantined").unwrap();
}

#[test]
fn archived_sources_delete_like_active_ones() {
    let (_dir, store, _token) = setup();
    store
        .create_source(&child("mcpdb_np_archived", "parent"))
        .unwrap();
    store
        .archive_source("mcpdb_np_archived", "admin", 9_500_700)
        .unwrap();
    assert_eq!(
        store.get_source("mcpdb_np_archived").unwrap().status,
        SourceStatus::Archived
    );
    store
        .delete_source("mcpdb_np_archived", "admin", 9_500_701)
        .unwrap();
    assert!(matches!(
        store.get_source("mcpdb_np_archived"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn deleting_an_unknown_source_is_not_found() {
    let (_dir, store, _token) = setup();
    assert!(matches!(
        store.delete_source("missing", "admin", 9_500_800),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn token_identity_survives_source_deletion() {
    let (_dir, store, token) = setup();
    store
        .create_source(&child("mcpdb_np_tokens", "parent"))
        .unwrap();
    store
        .delete_source("mcpdb_np_tokens", "admin", 9_500_900)
        .unwrap();
    // Deleting the source must not cascade into the creating token's identity.
    store.get_token_identity(&token).unwrap();
}
