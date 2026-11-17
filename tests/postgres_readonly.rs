//! Run only against an explicitly authorized, isolated PostgreSQL test database:
//! `cargo test --test postgres_readonly -- --ignored`
//!
//! Set MCPDB_TEST_PG_RO_DSN to a non-owner, non-superuser account with SELECT on
//! public.readonly_fixture(id bigint, name text), containing at least two rows.
//! The account must lack INSERT/UPDATE/DELETE and CREATE on schema public.
//! public.readonly_permission_probe must not exist. Provision the fixture outside
//! this test, without triggers/rules or concurrent writers; no setup/cleanup runs.
//!
//! Two layers are verified: the account itself denies writes even without any
//! application setting, and a query profile connection created by the service
//! denies the same operations inside its native read-only session.

use mcp_db::postgres::{connect, ProfileConnections};
use mcp_db::{CredentialResolver, Error, Profile, ProfilePurpose};
use tokio_postgres::{error::SqlState, Client, NoTls};

const FIXTURE_SQL: &str = "SELECT id, name FROM public.readonly_fixture ORDER BY id, name";
const SCHEMA_SQL: &str = "
    SELECT COALESCE(jsonb_agg(to_jsonb(snapshot) ORDER BY oid, attnum), '[]'::jsonb)
    FROM (
        SELECT c.oid, c.relname, c.relkind, a.attnum, a.attname,
               a.atttypid, a.atttypmod, a.attnotnull, a.attisdropped
        FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0
        WHERE n.nspname = 'public'
    ) snapshot";

const NEGATIVE_OPERATIONS: [(&str, &str); 4] = [
    (
        "INSERT",
        "INSERT INTO public.readonly_fixture (id, name)
         SELECT id, name FROM public.readonly_fixture ORDER BY id, name LIMIT 1",
    ),
    (
        "UPDATE",
        "UPDATE public.readonly_fixture SET name = 'readonly-denied'
         WHERE id IS NOT DISTINCT FROM
             (SELECT id FROM public.readonly_fixture ORDER BY id, name LIMIT 1)",
    ),
    (
        "DELETE",
        "DELETE FROM public.readonly_fixture
         WHERE id IS NOT DISTINCT FROM
             (SELECT id FROM public.readonly_fixture ORDER BY id, name LIMIT 1)",
    ),
    (
        "CREATE TABLE",
        "CREATE TABLE public.readonly_permission_probe (id bigint, name text)",
    ),
];

struct DsnResolver(String);

impl CredentialResolver for DsnResolver {
    fn resolve(&self, _reference: &str) -> Result<String, Error> {
        Ok(self.0.clone())
    }
}

fn test_dsn() -> String {
    std::env::var("MCPDB_TEST_PG_RO_DSN")
        .ok()
        .filter(|dsn| !dsn.trim().is_empty())
        .expect("set MCPDB_TEST_PG_RO_DSN to an authorized read-only test account")
}

fn query_profile() -> Profile {
    Profile {
        id: 1,
        source_id: "readonly".to_string(),
        name: "query".to_string(),
        purpose: ProfilePurpose::Query,
        credential_ref: "keyring:test/readonly".to_string(),
        enabled: true,
        version: 1,
        max_connections: Some(1),
        statement_timeout_ms: Some(5_000),
    }
}

async fn fixture_rows(client: &Client) -> Vec<(Option<i64>, Option<String>)> {
    client
        .query(FIXTURE_SQL, &[])
        .await
        .unwrap_or_else(|_| panic!("fixture SELECT failed"))
        .into_iter()
        .map(|row| {
            (
                row.try_get(0)
                    .unwrap_or_else(|_| panic!("fixture id must be bigint")),
                row.try_get(1)
                    .unwrap_or_else(|_| panic!("fixture name must be text")),
            )
        })
        .collect()
}

async fn schema_snapshot(client: &Client) -> serde_json::Value {
    client
        .query_one(SCHEMA_SQL, &[])
        .await
        .unwrap_or_else(|_| panic!("pg_catalog schema snapshot failed"))
        .get(0)
}

async fn assert_negative_matrix(
    client: &Client,
    fixture: &[(Option<i64>, Option<String>)],
    schema: &serde_json::Value,
) {
    for (operation, sql) in NEGATIVE_OPERATIONS {
        // Roll back even unexpected success; the transaction may inherit a read-only default.
        client
            .batch_execute("BEGIN")
            .await
            .unwrap_or_else(|_| panic!("negative request transaction failed"));
        let result = client.batch_execute(sql).await;
        client
            .batch_execute("ROLLBACK")
            .await
            .unwrap_or_else(|_| panic!("negative request rollback failed; stop testing"));
        let error = result
            .err()
            .unwrap_or_else(|| panic!("{operation} unexpectedly succeeded"));
        // 42501 proves the account lacks the privilege; 25006 proves the session is read-only.
        assert!(
            matches!(
                error.code(),
                Some(code)
                    if *code == SqlState::INSUFFICIENT_PRIVILEGE
                        || *code == SqlState::READ_ONLY_SQL_TRANSACTION
            ),
            "{operation} must be denied by PostgreSQL privileges or a read-only session"
        );
        // Compare without printing fixture contents on failure; SELECT must still work.
        assert!(
            fixture_rows(client).await == fixture,
            "fixture rows changed after {operation}"
        );
        assert!(
            schema_snapshot(client).await == *schema,
            "public schema changed after {operation}"
        );
    }
}

async fn assert_fixture_preconditions(
    client: &Client,
) -> (Vec<(Option<i64>, Option<String>)>, serde_json::Value) {
    let rows = fixture_rows(client).await;
    assert!(rows.len() >= 2, "fixture must contain at least two rows");
    let schema = schema_snapshot(client).await;
    let probe_exists: bool = client
        .query_one(
            "SELECT pg_catalog.to_regclass('public.readonly_permission_probe') IS NOT NULL",
            &[],
        )
        .await
        .unwrap_or_else(|_| panic!("DDL probe precondition SELECT failed"))
        .get(0);
    assert!(!probe_exists, "DDL probe relation must not already exist");
    (rows, schema)
}

#[tokio::test]
#[ignore = "requires explicit MCPDB_TEST_PG_RO_DSN and a pre-provisioned read-only fixture"]
async fn postgres_readonly_account_denies_dml_and_ddl_without_changing_fixture() {
    // Never print driver errors: they can contain connection or fixture details.
    let (client, connection) = tokio_postgres::connect(&test_dsn(), NoTls)
        .await
        .unwrap_or_else(|_| panic!("explicit read-only test connection failed"));
    let connection_task = tokio::spawn(connection);

    let (rows, schema) = assert_fixture_preconditions(&client).await;
    assert_negative_matrix(&client, &rows, &schema).await;

    drop(client);
    assert!(
        matches!(connection_task.await, Ok(Ok(()))),
        "read-only test connection did not close cleanly"
    );
}

#[tokio::test]
#[ignore = "requires explicit MCPDB_TEST_PG_RO_DSN and a pre-provisioned read-only fixture"]
async fn query_profile_connection_denies_dml_and_ddl_without_changing_fixture() {
    // Admission plus native read-only verification run inside `connect`, before any query.
    let resolver = DsnResolver(test_dsn());
    let connections = ProfileConnections::default();
    let client = connect(&resolver, &connections, &query_profile())
        .await
        .unwrap_or_else(|_| panic!("query profile connection did not pass read-only admission"));

    let readonly: String = client
        .query_one("SHOW transaction_read_only", &[])
        .await
        .unwrap_or_else(|_| panic!("read-only session state query failed"))
        .get(0);
    assert_eq!(readonly, "on", "query profile session must be read-only");

    let (rows, schema) = assert_fixture_preconditions(&client).await;
    assert_negative_matrix(&client, &rows, &schema).await;
    // Dropping the profile client aborts the connection task and releases the permit.
}
