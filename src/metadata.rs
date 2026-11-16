use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, NoTls};

use crate::credentials::CredentialResolver;
use crate::error::{Error, Result};
use crate::state::Profile;

/// Database metadata boundary. Read-only metadata queries only.
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    async fn list_tables(&self, profile: &Profile, schema: Option<&str>) -> Result<Value>;
    async fn describe_table(
        &self,
        profile: &Profile,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Value>;
    async fn explain(&self, profile: &Profile, sql: &str, params: &[Value]) -> Result<Value>;
}

/// Fails closed until credentials and a provider are configured.
pub struct UnavailableMetadata;

#[async_trait]
impl MetadataProvider for UnavailableMetadata {
    async fn list_tables(&self, _profile: &Profile, _schema: Option<&str>) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }

    async fn describe_table(
        &self,
        _profile: &Profile,
        _table: &str,
        _schema: Option<&str>,
    ) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }

    async fn explain(&self, _profile: &Profile, _sql: &str, _params: &[Value]) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }
}

/// Minimal PostgreSQL metadata provider. Pooling and limits belong to T-07.
pub struct PostgresMetadata {
    resolver: Arc<dyn CredentialResolver>,
}

impl PostgresMetadata {
    pub fn new(resolver: Arc<dyn CredentialResolver>) -> Self {
        Self { resolver }
    }

    async fn client(&self, profile: &Profile) -> Result<Client> {
        let dsn = self.resolver.resolve(&profile.credential_ref)?;
        let (client, connection) = tokio_postgres::connect(&dsn, NoTls)
            .await
            .map_err(|_| Error::MetadataUnavailable)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }
}

#[async_trait]
impl MetadataProvider for PostgresMetadata {
    async fn list_tables(&self, profile: &Profile, schema: Option<&str>) -> Result<Value> {
        let client = self.client(profile).await?;
        let params: [&(dyn ToSql + Sync); 1] = [&schema];
        let rows = client
            .query(
                "SELECT table_schema, table_name, table_type
                   FROM information_schema.tables
                  WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
                    AND ($1::text IS NULL OR table_schema = $1)
                  ORDER BY table_schema, table_name",
                &params,
            )
            .await
            .map_err(|_| Error::MetadataUnavailable)?;
        let tables = rows
            .iter()
            .map(|row| {
                json!({
                    "schema": row.get::<_, String>(0),
                    "name": row.get::<_, String>(1),
                    "kind": row.get::<_, String>(2),
                })
            })
            .collect::<Vec<_>>();
        Ok(Value::Array(tables))
    }

    async fn describe_table(
        &self,
        profile: &Profile,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Value> {
        let client = self.client(profile).await?;
        let params: [&(dyn ToSql + Sync); 2] = [&table, &schema];

        let columns = client
            .query(
                "SELECT column_name, data_type, is_nullable, column_default
                   FROM information_schema.columns
                  WHERE table_name = $1
                    AND table_schema = COALESCE($2::text, current_schema())
                  ORDER BY ordinal_position",
                &params,
            )
            .await
            .map_err(|_| Error::MetadataUnavailable)?
            .iter()
            .map(|row| {
                json!({
                    "name": row.get::<_, String>(0),
                    "type": row.get::<_, String>(1),
                    "nullable": row.get::<_, String>(2) == "YES",
                    "default": row.get::<_, Option<String>>(3),
                })
            })
            .collect::<Vec<_>>();

        let primary_key = client
            .query(
                "SELECT kcu.column_name
                   FROM information_schema.table_constraints tc
                   JOIN information_schema.key_column_usage kcu
                     ON tc.constraint_name = kcu.constraint_name
                    AND tc.table_schema = kcu.table_schema
                  WHERE tc.constraint_type = 'PRIMARY KEY'
                    AND tc.table_name = $1
                    AND tc.table_schema = COALESCE($2::text, current_schema())
                  ORDER BY kcu.ordinal_position",
                &params,
            )
            .await
            .map_err(|_| Error::MetadataUnavailable)?
            .iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>();

        let indexes = client
            .query(
                "SELECT indexname, indexdef
                   FROM pg_indexes
                  WHERE tablename = $1
                    AND schemaname = COALESCE($2::text, current_schema())
                  ORDER BY indexname",
                &params,
            )
            .await
            .map_err(|_| Error::MetadataUnavailable)?
            .iter()
            .map(|row| {
                json!({
                    "name": row.get::<_, String>(0),
                    "definition": row.get::<_, String>(1),
                })
            })
            .collect::<Vec<_>>();

        let foreign_keys = client
            .query(
                "SELECT tc.constraint_name, kcu.column_name,
                        ccu.table_schema, ccu.table_name, ccu.column_name
                   FROM information_schema.table_constraints tc
                   JOIN information_schema.key_column_usage kcu
                     ON tc.constraint_name = kcu.constraint_name
                    AND tc.table_schema = kcu.table_schema
                   JOIN information_schema.constraint_column_usage ccu
                     ON tc.constraint_name = ccu.constraint_name
                    AND tc.table_schema = ccu.table_schema
                  WHERE tc.constraint_type = 'FOREIGN KEY'
                    AND tc.table_name = $1
                    AND tc.table_schema = COALESCE($2::text, current_schema())
                  ORDER BY tc.constraint_name, kcu.ordinal_position",
                &params,
            )
            .await
            .map_err(|_| Error::MetadataUnavailable)?
            .iter()
            .map(|row| {
                json!({
                    "name": row.get::<_, String>(0),
                    "column": row.get::<_, String>(1),
                    "references_schema": row.get::<_, String>(2),
                    "references_table": row.get::<_, String>(3),
                    "references_column": row.get::<_, String>(4),
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "schema": schema,
            "table": table,
            "columns": columns,
            "primary_key": primary_key,
            "indexes": indexes,
            "foreign_keys": foreign_keys,
        }))
    }

    async fn explain(&self, _profile: &Profile, _sql: &str, _params: &[Value]) -> Result<Value> {
        // T-05 owns read-only/non-executing explain policy; do not execute here.
        Err(Error::MetadataUnavailable)
    }
}
