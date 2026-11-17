use mcp_db::{
    Driver, Environment, Error, NewSource, ProvisionLimits, ReservationStatus, SourceStatus, Store,
    Tool,
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
        .settle_reservation(first.id, ReservationStatus::Committed, None, now)
        .unwrap();

    assert!(matches!(
        store.reserve_provision(
            &token,
            "parent",
            "mcpdb_np_rateb",
            None,
            &limits,
            now + 59_999
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
            now + 60_000,
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
    assert!(matches!(
        store.settle_reservation(
            reservation.id,
            ReservationStatus::Committed,
            None,
            6_000_002
        ),
        Err(Error::Conflict(_))
    ));
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
