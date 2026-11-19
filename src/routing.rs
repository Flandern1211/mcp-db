use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::executor::{MutationOutcome, QueryOutcome, SqlExecutor};
use crate::metadata::MetadataProvider;
use crate::state::{Driver, Profile, Store};

/// Selects the execution adapter from the source driver. Authorization, routing and
/// approval stay driver-neutral; only the connection/SQL boundary is driver-specific.
#[derive(Clone)]
pub struct DriverExecutors {
    store: Arc<Mutex<Store>>,
    postgres_executor: Arc<dyn SqlExecutor>,
    postgres_metadata: Arc<dyn MetadataProvider>,
    sqlite_executor: Arc<dyn SqlExecutor>,
    sqlite_metadata: Arc<dyn MetadataProvider>,
}

impl DriverExecutors {
    pub fn new(
        store: Arc<Mutex<Store>>,
        postgres_executor: Arc<dyn SqlExecutor>,
        postgres_metadata: Arc<dyn MetadataProvider>,
        sqlite_executor: Arc<dyn SqlExecutor>,
        sqlite_metadata: Arc<dyn MetadataProvider>,
    ) -> Self {
        Self {
            store,
            postgres_executor,
            postgres_metadata,
            sqlite_executor,
            sqlite_metadata,
        }
    }

    fn driver(&self, profile: &Profile) -> Result<Driver> {
        let store = self.store.lock().map_err(|_| Error::MetadataUnavailable)?;
        Ok(store.get_source(&profile.source_id)?.driver)
    }

    fn executor(&self, profile: &Profile) -> Result<&Arc<dyn SqlExecutor>> {
        match self.driver(profile)? {
            Driver::Postgres => Ok(&self.postgres_executor),
            Driver::Sqlite => Ok(&self.sqlite_executor),
        }
    }

    fn metadata(&self, profile: &Profile) -> Result<&Arc<dyn MetadataProvider>> {
        match self.driver(profile)? {
            Driver::Postgres => Ok(&self.postgres_metadata),
            Driver::Sqlite => Ok(&self.sqlite_metadata),
        }
    }
}

#[async_trait]
impl SqlExecutor for DriverExecutors {
    async fn query(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<QueryOutcome> {
        self.executor(profile)?
            .query(profile, sql, params, max_rows)
            .await
    }

    async fn mutate(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
    ) -> Result<MutationOutcome> {
        self.executor(profile)?.mutate(profile, sql, params).await
    }

    async fn explain(&self, profile: &Profile, explain_sql: &str) -> Result<Value> {
        self.executor(profile)?.explain(profile, explain_sql).await
    }
}

#[async_trait]
impl MetadataProvider for DriverExecutors {
    async fn list_tables(&self, profile: &Profile, schema: Option<&str>) -> Result<Value> {
        self.metadata(profile)?.list_tables(profile, schema).await
    }

    async fn describe_table(
        &self,
        profile: &Profile,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Value> {
        self.metadata(profile)?
            .describe_table(profile, table, schema)
            .await
    }

    async fn explain(&self, profile: &Profile, sql: &str, params: &[Value]) -> Result<Value> {
        self.metadata(profile)?.explain(profile, sql, params).await
    }
}
