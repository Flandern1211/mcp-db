//! Read-only connectivity probe used by the admin profile editor.
//!
//! The tester receives an already-resolved DSN, opens a short-lived connection,
//! and returns non-secret server facts. It never writes and never logs the DSN.

use std::time::Duration;

use async_trait::async_trait;
use tokio_postgres::NoTls;

use crate::error::{Error, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub database: String,
    pub user: String,
    pub server_version: String,
}

#[async_trait]
pub trait ConnectionTester: Send + Sync {
    async fn test(&self, dsn: &str) -> Result<ConnectionInfo>;
}

pub struct PostgresConnectionTester;

#[async_trait]
impl ConnectionTester for PostgresConnectionTester {
    async fn test(&self, dsn: &str) -> Result<ConnectionInfo> {
        let _config: tokio_postgres::Config = dsn.parse().map_err(|_| {
            Error::InvalidInput("connection string is not a valid PostgreSQL DSN".to_string())
        })?;
        let (client, connection) =
            tokio::time::timeout(CONNECT_TIMEOUT, tokio_postgres::connect(dsn, NoTls))
                .await
                .map_err(|_| Error::ExecutionNotStarted("connection test timed out".to_string()))?
                .map_err(|error| {
                    Error::ExecutionNotStarted(format!("connection test failed: {error}"))
                })?;
        let connection_task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let row = tokio::time::timeout(
            QUERY_TIMEOUT,
            client.query_one(
                "SELECT current_database()::text, current_user::text,
                        current_setting('server_version')",
                &[],
            ),
        )
        .await
        .map_err(|_| Error::ExecutionNotStarted("connection test query timed out".to_string()))?
        .map_err(|error| {
            Error::ExecutionNotStarted(format!("connection test query failed: {error}"))
        })?;
        connection_task.abort();
        Ok(ConnectionInfo {
            database: row.get(0),
            user: row.get(1),
            server_version: row.get(2),
        })
    }
}
