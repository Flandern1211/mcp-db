use mcp_db::{
    Driver, Environment, Error, NewSource, ProfilePurpose, ProvisionLimits, ProvisionSpec,
    ReservationStatus, SourceStatus, Store, TokenIdentity, Tool,
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

#[test]
fn reserved_slots_count_against_capacity_and_committed_slots_stay_active() {
    let (_dir, store, token) = setup();
    let limits = limits(1, 1_000);
    let now = 1_000_000;

    let first = store
        .reserve_provision(&token, "parent", "mcpdb_np_first", None, &limits, now)
        .unwrap();
    assert_eq!(first.status, ReservationStatus::Reserved);
    assert!(matches!(
        store.reserve_provision(
            &token,
            "parent",
            "mcpdb_np_second",
            None,
            &limits,
            now + 5_000
        ),
        Err(Error::ResourceLimit(_))
    ));

    store
        .create_source(&child("mcpdb_np_first", "parent"))
        .unwrap();
    store
        .settle_reservation(first.id, ReservationStatus::Committed, None, now + 1)
        .unwrap();
    assert!(matches!(
        store.reserve_provision(
            &token,
            "parent",
            "mcpdb_np_second",
            None,
            &limits,
            now + 5_000
        ),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(
        store
            .provision_usage(&token, "parent", now + 5_000, 1_000)
            .unwrap(),
        mcp_db::state::ProvisionUsage {
            active: 1,
            recent: 0,
            quarantined: 0
        }
    );
}

#[test]
fn creation_rate_uses_reservation_and_settlement_timestamps() {
    let (_dir, store, token) = setup();
    let limits = limits(5, 60_000);
    let now = 2_000_000;
    let first = store
        .reserve_provision(&token, "parent", "mcpdb_np_ratea", None, &limits, now)
        .unwrap();
    store
        .create_source(&child("mcpdb_np_ratea", "parent"))
        .unwrap();
    store
        .settle_reservation(first.id, ReservationStatus::Committed, None, now + 50_000)
        .unwrap();

    // The success window starts at settlement, not at reservation time.
    assert!(matches!(
        store.reserve_provision(
            &token,
            "parent",
            "mcpdb_np_rateb",
            None,
            &limits,
            now + 60_000
        ),
        Err(Error::ResourceLimit(_))
    ));
    let second = store
        .reserve_provision(
            &token,
            "parent",
            "mcpdb_np_rateb",
            None,
            &limits,
            now + 110_001,
        )
        .unwrap();
    assert_eq!(second.status, ReservationStatus::Reserved);
}

#[test]
fn quarantine_blocks_new_databases_until_an_admin_settles_it() {
    let (_dir, store, token) = setup();
    let limits = limits(5, 1);
    let now = 3_000_000;
    let reservation = store
        .reserve_provision(&token, "parent", "mcpdb_np_quarantine", None, &limits, now)
        .unwrap();
    store
        .settle_reservation(
            reservation.id,
            ReservationStatus::Quarantined,
            Some("EXECUTION_UNKNOWN"),
            now + 1,
        )
        .unwrap();
    assert!(matches!(
        store.reserve_provision(&token, "parent", "mcpdb_np_next", None, &limits, now + 2),
        Err(Error::ProvisionBlocked(_))
    ));
    let quarantined = store.list_quarantined_reservations().unwrap();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(
        quarantined[0].reason_code.as_deref(),
        Some("EXECUTION_UNKNOWN")
    );

    store
        .settle_reservation(
            reservation.id,
            ReservationStatus::Released,
            Some("ADMIN_RELEASED"),
            now + 3,
        )
        .unwrap();
    store
        .reserve_provision(&token, "parent", "mcpdb_np_next", None, &limits, now + 4)
        .unwrap();
}

#[test]
fn archived_children_free_capacity_without_losing_the_commitment() {
    let (dir, store, token) = setup();
    let limits = limits(1, 1);
    let now = 4_000_000;
    let first = store
        .reserve_provision(&token, "parent", "mcpdb_np_archived", None, &limits, now)
        .unwrap();
    store
        .create_source(&child("mcpdb_np_archived", "parent"))
        .unwrap();
    store
        .settle_reservation(first.id, ReservationStatus::Committed, None, now)
        .unwrap();

    let connection = Connection::open(dir.path().join("state.db")).unwrap();
    connection
        .execute(
            "UPDATE sources SET status = 'archived', archived_at = ? WHERE id = ?",
            rusqlite::params![now, "mcpdb_np_archived"],
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        store
            .provision_usage(&token, "parent", now + 10, 1)
            .unwrap()
            .active,
        0
    );
    store
        .reserve_provision(&token, "parent", "mcpdb_np_after", None, &limits, now + 10)
        .unwrap();
    assert_eq!(
        store.get_source("mcpdb_np_archived").unwrap().status,
        SourceStatus::Archived
    );
}

#[test]
fn restart_quarantines_in_flight_reservations_and_records_audit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.db");
    let (token, reservation_id) = {
        let store = Store::open(&path).unwrap();
        store
            .create_source(&source("parent", Environment::NonProduction))
            .unwrap();
        let token = store.create_token("agent").unwrap().token_id;
        let reservation = store
            .reserve_provision(
                &token,
                "parent",
                "mcpdb_np_inflight",
                None,
                &limits(5, 1),
                5_000_000,
            )
            .unwrap();
        (token, reservation.id)
    };

    let store = Store::open(&path).unwrap();
    let reservation = store.get_reservation(reservation_id).unwrap();
    assert_eq!(reservation.status, ReservationStatus::Quarantined);
    assert_eq!(reservation.reason_code.as_deref(), Some("RESTART_UNKNOWN"));
    let connection = Connection::open(&path).unwrap();
    let audits: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM audit_events
              WHERE event = 'quarantined' AND token_id = ? AND tool = 'create_database'",
            [&token],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(audits, 1);
}

#[test]
fn settlement_rejects_invalid_transitions_and_unknown_rows() {
    let (_dir, store, token) = setup();
    let limits = limits(5, 1);
    let reservation = store
        .reserve_provision(
            &token,
            "parent",
            "mcpdb_np_settle",
            None,
            &limits,
            6_000_000,
        )
        .unwrap();
    store
        .settle_reservation(reservation.id, ReservationStatus::Released, None, 6_000_001)
        .unwrap();
    // A settled slot never returns to an active state, even through a commit.
    assert!(store
        .settle_reservation(
            reservation.id,
            ReservationStatus::Committed,
            None,
            6_000_002
        )
        .is_err());
    assert!(matches!(
        store.settle_reservation(reservation.id, ReservationStatus::Reserved, None, 6_000_003),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        store.settle_reservation(9_999, ReservationStatus::Released, None, 6_000_004),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn reservation_validates_parent_token_name_and_approval_ownership() {
    let (_dir, store, token) = setup();
    let limits = limits(5, 1);
    let now = 7_000_000;
    assert!(matches!(
        store.reserve_provision(&token, "missing", "mcpdb_np_a", None, &limits, now),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        store.reserve_provision(&token, "parent", "Invalid Name", None, &limits, now),
        Err(Error::InvalidInput(_))
    ));

    store
        .reserve_provision(&token, "parent", "mcpdb_np_dup", None, &limits, now)
        .unwrap();
    assert!(matches!(
        store.reserve_provision(&token, "parent", "mcpdb_np_dup", None, &limits, now),
        Err(Error::Conflict(_))
    ));

    let other = store.create_token("other").unwrap().token_id;
    let other_reservation = store
        .reserve_provision(&other, "parent", "mcpdb_np_other", None, &limits, now)
        .unwrap();
    assert!(other_reservation.approval_id.is_none());
    assert!(matches!(
        store.reserve_provision(&token, "parent", "mcpdb_np_own", Some(42), &limits, now),
        Err(Error::NotFound(_))
    ));
}

fn full_limits() -> ProvisionLimits {
    ProvisionLimits::new(
        5,
        1,
        vec![
            Tool::ListTables,
            Tool::QuerySql,
            Tool::MutateSql,
            Tool::ExecuteDdl,
        ],
    )
    .unwrap()
}

fn provision_input(
    spec: &ProvisionSpec,
    parent: &str,
    token_id: &str,
) -> mcp_db::state::ProvisionedSource {
    let identity = TokenIdentity {
        token_id: token_id.to_string(),
        agent: "agent".to_string(),
    };
    mcp_db::provision::result_source(spec, parent, &identity).unwrap()
}

#[test]
fn registration_commits_source_profiles_routes_grants_and_reservation() {
    let (_dir, store, token) = setup();
    let limits = full_limits();
    let spec =
        ProvisionSpec::generate(Environment::NonProduction, &limits, Some("scratch")).unwrap();
    let now = 8_000_000;
    let reservation = store
        .reserve_provision(&token, "parent", &spec.database, None, &limits, now)
        .unwrap();
    let input = provision_input(&spec, "parent", &token);

    let source = store
        .register_provisioned_source(&input, reservation.id, now + 1)
        .unwrap();
    assert_eq!(source.id, spec.database);
    assert_eq!(source.parent_source_id.as_deref(), Some("parent"));
    assert_eq!(source.environment, Environment::NonProduction);

    let profiles = store.list_profiles(&source.id).unwrap();
    for purpose in [
        ProfilePurpose::Query,
        ProfilePurpose::Mutate,
        ProfilePurpose::Schema,
    ] {
        assert!(profiles.iter().any(|profile| profile.purpose == purpose));
    }
    for tool in [
        Tool::ListTables,
        Tool::QuerySql,
        Tool::MutateSql,
        Tool::ExecuteDdl,
    ] {
        store
            .get_route(&source.id, tool)
            .unwrap_or_else(|_| panic!("missing route for {}", tool.as_str()));
    }
    let grants = store.list_grants_for_token(&token).unwrap();
    assert!(grants
        .iter()
        .any(|grant| grant.source_id == source.id && grant.tool == Tool::QuerySql));
    assert!(!grants
        .iter()
        .any(|grant| grant.source_id == source.id && grant.tool == Tool::CreateDatabase));
    assert_eq!(
        store.get_reservation(reservation.id).unwrap().status,
        ReservationStatus::Committed
    );
    assert_eq!(
        store
            .provision_usage(&token, "parent", now + 2, 1)
            .unwrap()
            .active,
        1
    );
}

#[test]
fn registration_failure_rolls_back_every_local_row() {
    let (_dir, store, token) = setup();
    let limits = full_limits();
    let spec = ProvisionSpec::generate(Environment::NonProduction, &limits, None).unwrap();
    let now = 8_100_000;
    let reservation = store
        .reserve_provision(&token, "parent", &spec.database, None, &limits, now)
        .unwrap();
    let mut input = provision_input(&spec, "parent", &token);
    input.routes[0].profile_name = "missing".to_string();

    assert!(store
        .register_provisioned_source(&input, reservation.id, now + 1)
        .is_err());
    assert!(store.get_source(&spec.database).is_err());
    assert!(store.list_profiles(&spec.database).unwrap().is_empty());
    assert_eq!(
        store.get_reservation(reservation.id).unwrap().status,
        ReservationStatus::Reserved
    );
}

#[test]
fn production_registration_rejects_write_grants_and_rolls_back() {
    let (_dir, store) = store();
    store
        .create_source(&source("prod", Environment::Production))
        .unwrap();
    let token = store.create_token("agent").unwrap().token_id;
    let limits = ProvisionLimits::new(5, 1, vec![Tool::QuerySql, Tool::MutateSql]).unwrap();
    let spec = ProvisionSpec::generate(Environment::Production, &limits, None).unwrap();
    assert!(spec.tools().unwrap().iter().all(|tool| !tool.is_write()));
    let now = 8_200_000;
    let reservation = store
        .reserve_provision(&token, "prod", &spec.database, None, &limits, now)
        .unwrap();
    let mut input = provision_input(&spec, "prod", &token);
    // A caller must not be able to smuggle a write grant into a production source.
    input.grants.push(Tool::MutateSql);

    assert!(store
        .register_provisioned_source(&input, reservation.id, now + 1)
        .is_err());
    assert!(store.get_source(&spec.database).is_err());
    assert_eq!(
        store.get_reservation(reservation.id).unwrap().status,
        ReservationStatus::Reserved
    );
}

fn provision_child(
    store: &Store,
    token: &str,
    parent: &str,
    limits: &ProvisionLimits,
    now: i64,
    label: &str,
) -> String {
    let spec = ProvisionSpec::generate(Environment::NonProduction, limits, Some(label)).unwrap();
    let reservation = store
        .reserve_provision(token, parent, &spec.database, None, limits, now)
        .unwrap();
    let input = provision_input(&spec, parent, token);
    store
        .register_provisioned_source(&input, reservation.id, now + 1)
        .unwrap();
    spec.database
}

fn pending_approval(child: &str, token: &str) -> mcp_db::NewApprovalRequest {
    mcp_db::NewApprovalRequest {
        draft: mcp_db::ApprovalDraft {
            token_id: token.to_string(),
            agent: "agent".to_string(),
            source_id: child.to_string(),
            source_environment: Environment::NonProduction,
            tool: Tool::MutateSql,
            sql: Some("UPDATE items SET name = 'x'".to_string()),
            params_json: "[]".to_string(),
            profile_id: 1,
            profile_version: 1,
            route_id: 1,
            route_version: 1,
            grant_policy: mcp_db::ApprovalPolicy::Approval,
            route_min_policy: mcp_db::ApprovalPolicy::Approval,
            high_risk: false,
        },
        channel: mcp_db::ApprovalChannel::Admin,
        status: mcp_db::ApprovalStatus::Pending,
        created_at: 9_000_000,
        expires_at: 9_120_000,
        decided_at: None,
        decided_by: None,
        reason_code: None,
        reason: None,
    }
}

#[test]
fn archive_hides_the_source_and_invalidates_pending_approvals() {
    let (_dir, store, token) = setup();
    let limits = full_limits();
    let child = provision_child(&store, &token, "parent", &limits, 9_000_000, "arch");
    let approval = store
        .insert_approval_request(&pending_approval(&child, &token))
        .unwrap();

    let archived = store.archive_source(&child, "admin", 9_000_002).unwrap();
    assert_eq!(archived.status, SourceStatus::Archived);
    assert_eq!(archived.archived_at, Some(9_000_002));
    assert!(store
        .list_visible_sources(&token)
        .unwrap()
        .iter()
        .all(|source| source.id != child));
    assert!(store.get_route(&child, Tool::QuerySql).is_ok());
    assert_ne!(store.count_grants_for_source(&child).unwrap(), 0);
    let record = store.get_approval(approval.id).unwrap();
    assert_eq!(record.status, mcp_db::ApprovalStatus::Expired);
    assert_eq!(record.reason_code.as_deref(), Some("SOURCE_ARCHIVED"));
    assert_eq!(
        store
            .reservation_by_database(&child)
            .unwrap()
            .expect("reservation")
            .status,
        ReservationStatus::Committed
    );
}

#[test]
fn restore_reactivates_inside_quota_and_rejects_excess() {
    let (_dir, store, token) = setup();
    let limits = ProvisionLimits::new(1, 1, vec![Tool::QuerySql]).unwrap();
    let first = provision_child(&store, &token, "parent", &limits, 9_100_000, "first");
    store.archive_source(&first, "admin", 9_100_002).unwrap();

    let restored = store
        .restore_source(&first, &limits, "admin", 9_100_003)
        .unwrap();
    assert_eq!(restored.status, SourceStatus::Active);
    assert_eq!(restored.archived_at, None);

    store.archive_source(&first, "admin", 9_100_004).unwrap();
    let second = provision_child(&store, &token, "parent", &limits, 9_100_005, "second");
    assert!(matches!(
        store.restore_source(&first, &limits, "admin", 9_100_006),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(
        store.get_source(&first).unwrap().status,
        SourceStatus::Archived
    );
    assert_eq!(
        store.get_source(&second).unwrap().status,
        SourceStatus::Active
    );
}

#[test]
fn restore_requires_profiles_and_grants() {
    let (dir, store, token) = setup();
    let limits = full_limits();
    let child = provision_child(&store, &token, "parent", &limits, 9_200_000, "grants");
    store.archive_source(&child, "admin", 9_200_002).unwrap();
    let connection = Connection::open(dir.path().join("state.db")).unwrap();
    connection
        .execute("DELETE FROM tool_grants WHERE source_id = ?", [&child])
        .unwrap();
    drop(connection);
    assert!(matches!(
        store.restore_source(&child, &limits, "admin", 9_200_003),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(
        store.get_source(&child).unwrap().status,
        SourceStatus::Archived
    );
}

#[test]
fn admin_open_does_not_recover_service_state() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.db");
    let reservation_id = {
        let store = Store::open(&path).unwrap();
        store
            .create_source(&source("parent", Environment::NonProduction))
            .unwrap();
        let token = store.create_token("agent").unwrap().token_id;
        store
            .reserve_provision(
                &token,
                "parent",
                "mcpdb_np_admin",
                None,
                &limits(5, 1),
                10_000_000,
            )
            .unwrap()
            .id
    };

    // A local admin command must not expire approvals or quarantine in-flight work.
    let admin = Store::open_admin(&path).unwrap();
    assert_eq!(
        admin.get_reservation(reservation_id).unwrap().status,
        ReservationStatus::Reserved
    );
    drop(admin);

    let service = Store::open(&path).unwrap();
    assert_eq!(
        service.get_reservation(reservation_id).unwrap().status,
        ReservationStatus::Quarantined
    );
}

#[test]
fn committing_without_a_registered_source_is_rejected() {
    let (_dir, store, token) = setup();
    let limits = limits(5, 1);
    let reservation = store
        .reserve_provision(
            &token,
            "parent",
            "mcpdb_np_nosource",
            None,
            &limits,
            11_000_000,
        )
        .unwrap();
    assert!(matches!(
        store.settle_reservation(
            reservation.id,
            ReservationStatus::Committed,
            None,
            11_000_001
        ),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(
        store.get_reservation(reservation.id).unwrap().status,
        ReservationStatus::Reserved
    );
}

#[test]
fn restore_requires_an_active_token_and_parent() {
    let (_dir, store, token) = setup();
    let limits = full_limits();
    let revoked = provision_child(&store, &token, "parent", &limits, 11_100_000, "revoked");
    store.archive_source(&revoked, "admin", 11_100_002).unwrap();
    store.revoke_token(&token).unwrap();
    assert!(matches!(
        store.restore_source(&revoked, &limits, "admin", 11_100_003),
        Err(Error::TokenRevoked)
    ));
    assert_eq!(
        store.get_source(&revoked).unwrap().status,
        SourceStatus::Archived
    );

    let other = store.create_token("agent-b").unwrap().token_id;
    let child = provision_child(&store, &other, "parent", &limits, 11_100_010, "parentgone");
    store.archive_source(&child, "admin", 11_100_012).unwrap();
    store.archive_source("parent", "admin", 11_100_013).unwrap();
    assert!(matches!(
        store.restore_source(&child, &limits, "admin", 11_100_014),
        Err(Error::ProvisionBlocked(_))
    ));
}
