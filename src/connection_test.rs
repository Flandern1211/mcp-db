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
                    Error::ExecutionNotStarted(format!(
                        "connection test failed: {}",
                        describe_error(&error)
                    ))
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
            Error::ExecutionNotStarted(format!(
                "connection test query failed: {}",
                describe_error(&error)
            ))
        })?;
        connection_task.abort();
        Ok(ConnectionInfo {
            database: row.get(0),
            user: row.get(1),
            server_version: row.get(2),
        })
    }
}

/// `tokio_postgres::Error`'s Display often only says "error connecting to
/// server"; the DNS/refused/auth cause sits in the source chain.
fn describe_error(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !parts.iter().any(|part| part == &text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ")
}
