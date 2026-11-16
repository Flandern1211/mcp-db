use std::time::{SystemTime, UNIX_EPOCH};

use mcp_db::{
    ApprovalChannel, ApprovalDraft, ApprovalOutcome, ApprovalStatus, Authorizer, ClaimOutcome,
    DecisionOutcome, Driver, Environment, NewApprovalRequest, NewGrant, NewProfile, NewRoute,
    NewSource, ProfilePurpose, Store, Tool,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn setup() -> (TempDir, Store, ApprovalDraft, String) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    let token = store.create_token("agent-a").expect("token");
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
        .expect("source");
    let profile = store
        .create_profile(&NewProfile {
            source_id: "a".to_string(),
            name: "mutate".to_string(),
            purpose: ProfilePurpose::Mutate,
            credential_ref: "keyring:mcp-db/test/query".to_string(),
            max_connections: Some(1),
            statement_timeout_ms: Some(1_000),
        })
        .expect("profile");
    let route = store
        .create_route(&NewRoute {
            source_id: "a".to_string(),
            tool: Tool::MutateSql,
            profile_id: profile.id,
            min_approval_policy: mcp_db::ApprovalPolicy::Direct,
        })
        .expect("route");
    store
        .create_grant(&NewGrant {
            token_id: token.token_id.clone(),
            source_id: "a".to_string(),
            tool: Tool::MutateSql,
            policy: Some(mcp_db::ApprovalPolicy::Approval),
        })
        .expect("grant");
    let call = Authorizer::new(&store)
        .authorize(&token.token_id, Some("a"), Tool::MutateSql)
        .expect("authorize");
    let grant = store
        .get_grant(&token.token_id, "a", Tool::MutateSql)
        .expect("grant row");
    let draft = ApprovalDraft {
        token_id: token.token_id.clone(),
        agent: token.agent.clone(),
        source_id: "a".to_string(),
        source_environment: Environment::NonProduction,
        tool: Tool::MutateSql,
        sql: Some("UPDATE users SET name = 'x' WHERE id = 1".to_string()),
        params_json: "[]".to_string(),
        profile_id: profile.id,
        profile_version: profile.version,
        route_id: route.id,
        route_version: route.version,
        grant_policy: grant.policy,
        route_min_policy: call.route.min_approval_policy,
        high_risk: false,
    };
    (dir, store, draft, token.token_id)
}

fn pending(draft: &ApprovalDraft, now: i64, expires_at: i64) -> NewApprovalRequest {
    NewApprovalRequest {
        draft: draft.clone(),
        channel: ApprovalChannel::Admin,
        status: ApprovalStatus::Pending,
        created_at: now,
        expires_at,
        decided_at: None,
        decided_by: None,
        reason_code: None,
        reason: None,
    }
}

#[test]
fn approval_is_approved_then_consumed_once() {
    let (_dir, store, draft, token_id) = setup();
    let now = now();
    let record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("insert");
    assert_eq!(record.status, ApprovalStatus::Pending);

    assert_eq!(
        store
            .decide_approval(record.id, true, "admin", now, 60_000)
            .expect("decide"),
        DecisionOutcome::Approved
    );
    let claimed = store
        .claim_approval(record.id, &token_id, &draft.digest(), now)
        .expect("claim");
    let ClaimOutcome::Claimed(claimed) = claimed else {
        panic!("expected claim");
    };
    assert_eq!(claimed.status, ApprovalStatus::Consumed);

    assert_eq!(
        store
            .claim_approval(record.id, &token_id, &draft.digest(), now)
            .expect("second claim"),
        ClaimOutcome::NotApproved
    );
    assert!(store
        .finish_approval(record.id, ApprovalOutcome::Succeeded, Some(1), None, now)
        .expect("finish"));
    assert!(!store
        .finish_approval(record.id, ApprovalOutcome::Failed, None, Some("x"), now)
        .expect("second finish"));
    assert!(store.count_audit_events(record.id).expect("audit") >= 4);
}

#[test]
fn digest_mismatch_and_expired_decisions_fail_closed() {
    let (_dir, store, draft, token_id) = setup();
    let now = now();
    let record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("insert");
    store
        .decide_approval(record.id, true, "admin", now, 60_000)
        .expect("decide");
    assert_eq!(
        store
            .claim_approval(record.id, &token_id, "0000", now)
            .expect("claim"),
        ClaimOutcome::DigestMismatch
    );

    let expired = store
        .insert_approval_request(&pending(&draft, now, now - 1))
        .expect("insert expired");
    assert_eq!(
        store
            .decide_approval(expired.id, true, "admin", now, 60_000)
            .expect("decide expired"),
        DecisionOutcome::Expired
    );
    assert_eq!(
        store.get_approval(expired.id).expect("get").status,
        ApprovalStatus::Expired
    );
}

#[test]
fn token_revoke_invalidates_pending_and_approved() {
    let (_dir, store, draft, token_id) = setup();
    let now = now();
    let record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("insert");
    store.revoke_token(&token_id).expect("revoke");
    assert_eq!(
        store.get_approval(record.id).expect("get").status,
        ApprovalStatus::Expired
    );
    assert!(store.count_audit_events(record.id).expect("audit") >= 2);
}

#[test]
fn restart_expires_unconsumed_and_marks_inflight_unknown() {
    let (dir, store, draft, token_id) = setup();
    let now = now();
    let pending_record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("pending");
    let approved_record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("approved");
    store
        .decide_approval(approved_record.id, true, "admin", now, 60_000)
        .expect("decide");
    let consumed_record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("consumed");
    store
        .decide_approval(consumed_record.id, true, "admin", now, 60_000)
        .expect("decide consumed");
    let claimed = store
        .claim_approval(consumed_record.id, &token_id, &draft.digest(), now)
        .expect("claim");
    assert!(matches!(claimed, ClaimOutcome::Claimed(_)));
    drop(store);

    let store = Store::open(dir.path().join("state.db")).expect("reopen");
    assert_eq!(
        store.get_approval(pending_record.id).expect("get").status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        store.get_approval(approved_record.id).expect("get").status,
        ApprovalStatus::Expired
    );
    let consumed = store.get_approval(consumed_record.id).expect("get");
    assert_eq!(consumed.status, ApprovalStatus::Consumed);
    assert_eq!(consumed.outcome, Some(ApprovalOutcome::Unknown));
}

#[test]
fn route_change_invalidates_approved_request() {
    let (dir, store, draft, token_id) = setup();
    let now = now();
    let record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("insert");
    store
        .decide_approval(record.id, true, "admin", now, 60_000)
        .expect("decide");

    let connection = Connection::open(dir.path().join("state.db")).expect("raw state");
    connection
        .execute(
            "UPDATE source_tool_routes SET version = version + 1 WHERE id = ?",
            [draft.route_id],
        )
        .expect("change route");
    drop(connection);

    let claim = store
        .claim_approval(record.id, &token_id, &draft.digest(), now)
        .expect("claim");
    assert_eq!(claim, ClaimOutcome::Stale("ROUTE_CHANGED".to_string()));
    assert_eq!(
        store.get_approval(record.id).expect("get").status,
        ApprovalStatus::Expired
    );
}

#[test]
fn unknown_result_is_terminal_and_not_overwritten() {
    let (_dir, store, draft, token_id) = setup();
    let now = now();
    let record = store
        .insert_approval_request(&pending(&draft, now, now + 60_000))
        .expect("insert");
    store
        .decide_approval(record.id, true, "admin", now, 60_000)
        .expect("decide");
    store
        .claim_approval(record.id, &token_id, &draft.digest(), now)
        .expect("claim");
    assert!(store
        .finish_approval(
            record.id,
            ApprovalOutcome::Unknown,
            None,
            Some("EXECUTION_UNKNOWN"),
            now
        )
        .expect("finish unknown"));
    assert!(!store
        .finish_approval(record.id, ApprovalOutcome::Succeeded, Some(1), None, now)
        .expect("finish again"));
    let record = store.get_approval(record.id).expect("get");
    assert_eq!(record.outcome, Some(ApprovalOutcome::Unknown));
}
