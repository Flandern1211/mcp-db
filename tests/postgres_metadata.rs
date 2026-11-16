use std::sync::Arc;

use mcp_db::{
    CredentialResolver, Error, MetadataProvider, PostgresMetadata, Profile, ProfilePurpose,
};

struct EmptyResolver;

impl CredentialResolver for EmptyResolver {
    fn resolve(&self, _reference: &str) -> Result<String, Error> {
        Err(Error::CredentialUnavailable)
    }
}

struct StaticResolver(String);

impl CredentialResolver for StaticResolver {
    fn resolve(&self, _reference: &str) -> Result<String, Error> {
        Ok(self.0.clone())
    }
}

fn profile() -> Profile {
    Profile {
        id: 1,
        source_id: "test".to_string(),
        name: "query".to_string(),
        purpose: ProfilePurpose::Query,
        credential_ref: "keyring:mcp-db/test/query".to_string(),
        enabled: true,
        version: 1,
        max_connections: None,
        statement_timeout_ms: None,
    }
}

#[tokio::test]
async fn missing_credential_fails_before_connecting() {
    let provider = PostgresMetadata::new(Arc::new(EmptyResolver));
    let result = provider.list_tables(&profile(), None).await;
    assert!(matches!(result, Err(Error::CredentialUnavailable)));
}

#[tokio::test]
#[ignore = "requires MCPDB_TEST_PG_DSN and a read-only PostgreSQL test account"]
async fn real_postgres_metadata_reads_tables_and_columns() {
    let dsn = std::env::var("MCPDB_TEST_PG_DSN")
        .expect("MCPDB_TEST_PG_DSN must point at a controlled PostgreSQL test database");
    let provider = PostgresMetadata::new(Arc::new(StaticResolver(dsn)));
    let tables = provider
        .list_tables(&profile(), None)
        .await
        .expect("tables");
    let tables = tables.as_array().expect("tables array");
    if let Some(first) = tables.first() {
        let schema = first["schema"].as_str();
        let name = first["name"].as_str().expect("table name");
        let description = provider
            .describe_table(&profile(), name, schema)
            .await
            .expect("describe table");
        assert!(description["columns"].is_array());
    }
}
