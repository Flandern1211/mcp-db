use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use crate::credentials::CredentialResolver;
use crate::error::{Error, Result};
use crate::state::{Profile, ProfilePurpose};

pub const DEFAULT_MAX_CONNECTIONS: u32 = 5;
pub const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 30_000;

struct Capacity {
    size: u32,
    permits: Arc<Semaphore>,
}

/// Shared admission control for SQL and metadata connections, keyed by exact profile.
#[derive(Default)]
pub struct ProfileConnections {
    profiles: Mutex<HashMap<(String, i64), Capacity>>,
}

impl ProfileConnections {
    pub fn acquire(&self, profile: &Profile) -> Result<OwnedSemaphorePermit> {
        let size = profile.max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS);
        validate_limits(profile.max_connections, profile.statement_timeout_ms)?;
        if !profile.enabled {
            return Err(Error::ProfileDisabled(profile.id));
        }
        if profile.purpose == ProfilePurpose::Provision {
            return Err(Error::InvalidInput(
                "provision profiles cannot enter the ordinary database path".to_string(),
            ));
        }
        let mut profiles = self.profiles.lock().map_err(|_| {
            Error::ResourceLimit("profile admission control unavailable".to_string())
        })?;
        let key = (profile.source_id.clone(), profile.id);
        let capacity = profiles.entry(key).or_insert_with(|| Capacity {
            size,
            permits: Arc::new(Semaphore::new(size as usize)),
        });
        if capacity.size != size {
            if capacity.permits.available_permits() != capacity.size as usize {
                return Err(Error::ResourceLimit(
                    "profile capacity changed while connections remain active".to_string(),
                ));
            }
            *capacity = Capacity {
                size,
                permits: Arc::new(Semaphore::new(size as usize)),
            };
        }
        capacity
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::ResourceLimit("profile connection capacity exhausted".to_string()))
    }
}

pub fn validate_limits(connections: Option<u32>, timeout: Option<u64>) -> Result<()> {
    if connections.is_some_and(|value| value == 0 || value > 1_000) {
        return Err(Error::InvalidInput(
            "max_connections must be between 1 and 1000".to_string(),
        ));
    }
    if timeout.is_some_and(|value| value == 0 || value > i32::MAX as u64) {
        return Err(Error::InvalidInput(
            "statement_timeout_ms must be between 1 and 2147483647".to_string(),
        ));
    }
    Ok(())
}

/// The permit lives until the connection task is closed, including cancellation.
pub struct ProfileClient {
    client: Client,
    connection: JoinHandle<()>,
}

impl Deref for ProfileClient {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl Drop for ProfileClient {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

pub async fn connect(
    resolver: &dyn CredentialResolver,
    connections: &ProfileConnections,
    profile: &Profile,
) -> Result<ProfileClient> {
    // Admission happens before credential resolution or any database connection.
    let permit = connections.acquire(profile)?;
    let dsn = resolver.resolve(&profile.credential_ref)?;
    let (client, connection) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_postgres::connect(&dsn, NoTls),
    )
    .await
    .map_err(|_| Error::ResourceLimit("database connection timed out".to_string()))?
    .map_err(|_| Error::MetadataUnavailable)?;
    let connection = tokio::spawn(async move {
        let _permit = permit;
        let _ = connection.await;
    });
    let client = ProfileClient { client, connection };
    let timeout = profile
        .statement_timeout_ms
        .unwrap_or(DEFAULT_STATEMENT_TIMEOUT_MS)
        .to_string();
    tokio::time::timeout(
        Duration::from_secs(10),
        client.query_one(
            "SELECT set_config('statement_timeout', $1, false)",
            &[&timeout],
        ),
    )
    .await
    .map_err(|_| Error::ResourceLimit("database session initialization timed out".to_string()))?
    .map_err(database_error)?;
    if profile.purpose == ProfilePurpose::Query {
        tokio::time::timeout(Duration::from_secs(10), verify_query_session(&client))
            .await
            .map_err(|_| {
                Error::ResourceLimit("query profile verification timed out".to_string())
            })??;
    }
    Ok(client)
}

async fn verify_query_session(client: &Client) -> Result<()> {
    client
        .batch_execute("SET default_transaction_read_only = on")
        .await
        .map_err(database_error)?;
    let readonly: String = client
        .query_one("SHOW transaction_read_only", &[])
        .await
        .map_err(database_error)?
        .get(0);
    if readonly != "on" {
        return Err(Error::InvalidInput(
            "query profile is not read-only".to_string(),
        ));
    }
    // Empty catalog predicates cannot modify data; rollback protects even unexpected success.
    for sql in [
        "INSERT INTO pg_catalog.pg_class SELECT * FROM pg_catalog.pg_class WHERE false",
        "UPDATE pg_catalog.pg_class SET relname = relname WHERE false",
        "DELETE FROM pg_catalog.pg_class WHERE false",
        "CREATE TABLE public.mcpdb_readonly_probe (id integer)",
    ] {
        client
            .batch_execute("BEGIN READ ONLY")
            .await
            .map_err(database_error)?;
        let result = client.batch_execute(sql).await;
        client
            .batch_execute("ROLLBACK")
            .await
            .map_err(database_error)?;
        match result {
            Err(error) if matches!(error.code(), Some(code) if *code == tokio_postgres::error::SqlState::READ_ONLY_SQL_TRANSACTION || *code == tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE) =>
                {}
            _ => {
                return Err(Error::InvalidInput(
                    "query profile failed native read-only verification".to_string(),
                ))
            }
        }
    }
    Ok(())
}

pub fn database_error(error: tokio_postgres::Error) -> Error {
    match error.code() {
        Some(code) if *code == tokio_postgres::error::SqlState::QUERY_CANCELED => {
            Error::ResourceLimit("PostgreSQL statement timeout or cancellation".to_string())
        }
        Some(code) if *code == tokio_postgres::error::SqlState::UNIQUE_VIOLATION => {
            Error::Conflict("unique constraint violation".to_string())
        }
        _ => Error::MetadataUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile {
            id: 1,
            source_id: "test".to_string(),
            name: "query".to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: "keyring:test".to_string(),
            enabled: true,
            version: 1,
            max_connections: Some(1),
            statement_timeout_ms: Some(50),
        }
    }

    #[test]
    fn same_profile_shares_capacity_across_versions() {
        let registry = ProfileConnections::default();
        let mut profile = profile();
        let permit = registry.acquire(&profile).unwrap();
        profile.version += 1;
        assert!(matches!(
            registry.acquire(&profile),
            Err(Error::ResourceLimit(_))
        ));
        drop(permit);
        assert!(registry.acquire(&profile).is_ok());
    }

    #[test]
    fn capacity_change_does_not_create_an_unlimited_second_bucket() {
        let registry = ProfileConnections::default();
        let mut profile = profile();
        let permit = registry.acquire(&profile).unwrap();
        profile.max_connections = Some(2);
        assert!(registry.acquire(&profile).is_err());
        drop(permit);
        let first = registry.acquire(&profile).unwrap();
        let second = registry.acquire(&profile).unwrap();
        assert!(registry.acquire(&profile).is_err());
        drop((first, second));
    }

    #[test]
    fn invalid_limits_and_provision_are_rejected() {
        assert!(validate_limits(Some(0), None).is_err());
        assert!(validate_limits(Some(1001), None).is_err());
        assert!(validate_limits(None, Some(u64::MAX)).is_err());
        let mut profile = profile();
        profile.purpose = ProfilePurpose::Provision;
        assert!(ProfileConnections::default().acquire(&profile).is_err());
    }
}
