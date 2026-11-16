use serde_json::Value;

use crate::error::{Error, Result};
use crate::state::Profile;

/// Database metadata boundary. The PostgreSQL adapter is supplied by the driver task.
pub trait MetadataProvider: Send + Sync {
    fn list_tables(&self, profile: &Profile, schema: Option<&str>) -> Result<Value>;
    fn describe_table(&self, profile: &Profile, table: &str, schema: Option<&str>)
        -> Result<Value>;
    fn explain(&self, profile: &Profile, sql: &str, params: &[Value]) -> Result<Value>;
}

/// Fails closed until a PostgreSQL provider and credential resolver are configured.
pub struct UnavailableMetadata;

impl MetadataProvider for UnavailableMetadata {
    fn list_tables(&self, _profile: &Profile, _schema: Option<&str>) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }

    fn describe_table(
        &self,
        _profile: &Profile,
        _table: &str,
        _schema: Option<&str>,
    ) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }

    fn explain(&self, _profile: &Profile, _sql: &str, _params: &[Value]) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }
}
