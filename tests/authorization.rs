use std::path::PathBuf;

use mcp_db::{
    ApprovalPolicy, Authorizer, Driver, Environment, Error, NewGrant, NewProfile, NewRoute,
    NewSource, ProfilePurpose, Store, Tool,
};
use rusqlite::{params, Connection};
use tempfile::TempDir;

fn setup() -> (TempDir, Store, String, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("state.db");
    let store = Store::open(&path).expect("open store");
    let token = store.create_token("agent-a").expect("issue token");
    (dir, store, token.token_id, path)
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
        credential_ref: format!("keyring:mcp-db/test/{source_id}/{name}"),
        max_connections: Some(5),
        statement_timeout_ms: Some(1_000),
    }
}

fn add_route(
    store: &Store,
    source_id: &str,
    tool: Tool,
    purpose: ProfilePurpose,
    policy: ApprovalPolicy,
) -> i64 {
    let profile = store
        .create_profile(&profile(source_id, purpose.as_str(), purpose))
        .expect("create profile");
    store
        .create_route(&NewRoute {
            source_id: source_id.to_string(),
            tool,
            profile_id: profile.id,
            min_approval_policy: policy,
        })
        .expect("create route");
    profile.id
}

fn add_grant(
    store: &Store,
    token_id: &str,
    source_id: &str,
    tool: Tool,
    policy: Option<ApprovalPolicy>,
) {
    store
        .create_grant(&NewGrant {
            token_id: token_id.to_string(),
            source_id: source_id.to_string(),
            tool,
            policy,
        })
        .expect("create grant");
}

#[test]
fn visible_tools_are_the_union_and_calls_still_check_exact_grants() {
    let (_dir, store, token, _path) = setup();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    store
        .create_source(&source("b", Environment::NonProduction))
        .unwrap();
    let a_profile = add_route(
        &store,
        "a",
        Tool::QuerySql,
        ProfilePurpose::Query,
        ApprovalPolicy::Direct,
    );
    add_route(
        &store,
        "b",
        Tool::MutateSql,
        ProfilePurpose::Mutate,
        ApprovalPolicy::Direct,
    );
    add_grant(&store, &token, "a", Tool::QuerySql, None);
    add_grant(&store, &token, "b", Tool::MutateSql, None);

    let authorizer = Authorizer::new(&store);
    assert_eq!(
        authorizer
            .visible_sources(&token)
            .unwrap()
            .into_iter()
            .map(|source| source.id)
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(
        authorizer.visible_tools(&token).unwrap(),
        vec![Tool::ListSources, Tool::QuerySql, Tool::MutateSql]
    );

    let call = authorizer
        .authorize(&token, Some("a"), Tool::QuerySql)
        .unwrap();
    assert_eq!(call.profile.id, a_profile);
    assert_eq!(call.policy, ApprovalPolicy::Direct);

    assert!(matches!(
        authorizer.authorize(&token, Some("b"), Tool::QuerySql),
        Err(Error::ToolNotGranted(_))
    ));
}

#[test]
fn source_is_required_only_when_multiple_sources_are_visible() {
    let (_dir, store, token, _path) = setup();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    add_route(
        &store,
        "a",
        Tool::QuerySql,
        ProfilePurpose::Query,
        ApprovalPolicy::Direct,
    );
    add_grant(&store, &token, "a", Tool::QuerySql, None);

    let authorizer = Authorizer::new(&store);
    assert_eq!(
        authorizer
            .authorize(&token, None, Tool::QuerySql)
            .unwrap()
            .source
            .id,
        "a"
    );

    store
        .create_source(&source("b", Environment::NonProduction))
        .unwrap();
    add_route(
        &store,
        "b",
        Tool::QuerySql,
        ProfilePurpose::Query,
        ApprovalPolicy::Direct,
    );
    add_grant(&store, &token, "b", Tool::QuerySql, None);
    assert!(matches!(
        authorizer.authorize(&token, None, Tool::QuerySql),
        Err(Error::SourceRequired)
    ));
}

#[test]
fn missing_route_is_rejected_without_a_database_call() {
    let (_dir, store, token, _path) = setup();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    add_grant(&store, &token, "a", Tool::QuerySql, None);

    assert!(matches!(
        Authorizer::new(&store).authorize(&token, Some("a"), Tool::QuerySql),
        Err(Error::RouteNotFound(_))
    ));
}

#[test]
fn disabled_profile_and_archived_source_are_rejected() {
    let (_dir, store, token, path) = setup();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    let profile_id = add_route(
        &store,
        "a",
        Tool::QuerySql,
        ProfilePurpose::Query,
        ApprovalPolicy::Direct,
    );
    add_grant(&store, &token, "a", Tool::QuerySql, None);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE connection_profiles SET enabled = 0 WHERE id = ?",
            [profile_id],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        Authorizer::new(&store).authorize(&token, Some("a"), Tool::QuerySql),
        Err(Error::ProfileDisabled(_))
    ));

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE connection_profiles SET enabled = 1 WHERE id = ?",
            [profile_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sources SET status = 'archived', archived_at = 1 WHERE id = 'a'",
            [],
        )
        .unwrap();
    drop(connection);
    let authorizer = Authorizer::new(&store);
    assert!(matches!(
        authorizer.authorize(&token, Some("a"), Tool::QuerySql),
        Err(Error::SourceArchived(_))
    ));
    assert!(authorizer.visible_sources(&token).unwrap().is_empty());
}

#[test]
fn production_write_is_rejected_even_if_a_grant_row_exists() {
    let (_dir, store, token, path) = setup();
    store
        .create_source(&source("prod", Environment::Production))
        .unwrap();
    add_route(
        &store,
        "prod",
        Tool::MutateSql,
        ProfilePurpose::Mutate,
        ApprovalPolicy::Approval,
    );
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO tool_grants (token_id, source_id, tool, policy, created_at)
             VALUES (?, 'prod', 'mutate_sql', 'direct', 1)",
            params![token],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        Authorizer::new(&store).authorize(&token, Some("prod"), Tool::MutateSql),
        Err(Error::ProductionWriteForbidden)
    ));
}

#[test]
fn route_minimum_policy_is_stricter_than_grant_policy() {
    let (_dir, store, token, _path) = setup();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    add_route(
        &store,
        "a",
        Tool::MutateSql,
        ProfilePurpose::Mutate,
        ApprovalPolicy::Approval,
    );
    add_grant(
        &store,
        &token,
        "a",
        Tool::MutateSql,
        Some(ApprovalPolicy::Direct),
    );

    let call = Authorizer::new(&store)
        .authorize(&token, Some("a"), Tool::MutateSql)
        .unwrap();
    assert_eq!(call.policy, ApprovalPolicy::Approval);
}

#[test]
fn newly_added_grants_are_visible_without_restarting_or_caching() {
    let (_dir, store, token, _path) = setup();
    store
        .create_source(&source("a", Environment::NonProduction))
        .unwrap();
    add_route(
        &store,
        "a",
        Tool::QuerySql,
        ProfilePurpose::Query,
        ApprovalPolicy::Direct,
    );

    let authorizer = Authorizer::new(&store);
    assert_eq!(
        authorizer.visible_tools(&token).unwrap(),
        vec![Tool::ListSources]
    );
    add_grant(&store, &token, "a", Tool::QuerySql, None);
    assert_eq!(
        authorizer.visible_tools(&token).unwrap(),
        vec![Tool::ListSources, Tool::QuerySql]
    );
}

#[test]
fn revoked_and_unknown_tokens_cannot_see_or_call() {
    let (_dir, store, token, _path) = setup();
    store.revoke_token(&token).unwrap();
    let authorizer = Authorizer::new(&store);
    assert!(matches!(
        authorizer.visible_tools(&token),
        Err(Error::TokenRevoked)
    ));
    assert!(matches!(
        authorizer.authorize(&token, Some("a"), Tool::QuerySql),
        Err(Error::TokenRevoked)
    ));
    assert!(matches!(
        authorizer.visible_tools("tok_missing"),
        Err(Error::NotFound(_))
    ));
}
