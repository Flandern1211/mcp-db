use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mcp_db::postgres::{connect, ProfileConnections};
use mcp_db::{
    CredentialResolver, Error, MetadataProvider, PostgresExecutor, PostgresMetadata, Profile,
    ProfilePurpose, SqlExecutor,
};

struct TestResolver {
    dsn: String,
    calls: AtomicUsize,
}

impl CredentialResolver for TestResolver {
    fn resolve(&self, _reference: &str) -> Result<String, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.dsn.clone())
    }
}

fn profile() -> Profile {
    Profile {
        id: 1,
        source_id: "readonly".to_string(),
        name: "query".to_string(),
        purpose: ProfilePurpose::Query,
        credential_ref: "keyring:test/readonly".to_string(),
        enabled: true,
        version: 1,
        max_connections: Some(1),
        statement_timeout_ms: Some(50),
    }
}

#[tokio::test]
async fn capacity_is_checked_before_credentials_or_network() {
    let connections = Arc::new(ProfileConnections::default());
    let profile = profile();
    let permit = connections.acquire(&profile).unwrap();
    let resolver = Arc::new(TestResolver {
        dsn: "not-a-real-dsn".to_string(),
        calls: AtomicUsize::new(0),
    });
    let executor = PostgresExecutor::with_connections(resolver.clone(), connections.clone());
    let metadata = PostgresMetadata::with_connections(resolver.clone(), connections);
    assert!(matches!(
        executor.query(&profile, "SELECT 1", &[], 1).await,
        Err(Error::ResourceLimit(_))
    ));
    assert!(matches!(
        metadata.list_tables(&profile, None).await,
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    drop(permit);
}

#[tokio::test]
#[ignore = "requires explicit MCPDB_TEST_PG_RO_DSN"]
async fn postgres_profile_capacity_timeout_and_release_are_enforced() {
    let dsn = std::env::var("MCPDB_TEST_PG_RO_DSN").expect("explicit test DSN required");
    let resolver = Arc::new(TestResolver {
        dsn,
        calls: AtomicUsize::new(0),
    });
    let connections = Arc::new(ProfileConnections::default());
    let profile = profile();
    let executor = PostgresExecutor::with_connections(resolver.clone(), connections.clone());
    let metadata = PostgresMetadata::with_connections(resolver.clone(), connections.clone());

    let held = connect(resolver.as_ref(), &connections, &profile)
        .await
        .unwrap();
    assert!(matches!(
        metadata.list_tables(&profile, None).await,
        Err(Error::ResourceLimit(_))
    ));
    assert!(matches!(
        executor.query(&profile, "SELECT 1", &[], 1).await,
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let timeout: String = held
        .query_one("SHOW statement_timeout", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(timeout, "50ms");
    let error = held
        .query("SELECT pg_sleep(0.2)", &[])
        .await
        .expect_err("server timeout required");
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
    );
    assert!(held.query_one("SELECT 1", &[]).await.is_ok());
    let error = held
        .execute("UPDATE public.readonly_fixture SET name = 'blocked'", &[])
        .await
        .expect_err("native read-only session must deny writes");
    assert!(
        matches!(error.code(), Some(code) if *code == tokio_postgres::error::SqlState::READ_ONLY_SQL_TRANSACTION || *code == tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
    );
    drop(held);
    tokio::task::yield_now().await;

    assert!(matches!(
        executor
            .query(&profile, "SELECT pg_sleep(0.2)", &[], 1)
            .await,
        Err(Error::ResourceLimit(_))
    ));
    tokio::task::yield_now().await;
    assert!(executor
        .query(&profile, "SELECT 1 AS value", &[], 1)
        .await
        .is_ok());
    tokio::task::yield_now().await;
    assert!(metadata.list_tables(&profile, Some("public")).await.is_ok());
}
