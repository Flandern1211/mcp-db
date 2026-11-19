use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Postgres,
    Sqlite,
}

impl Driver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "postgres" => Ok(Self::Postgres),
            "sqlite" => Ok(Self::Sqlite),
            _ => Err(Error::InvalidInput(format!("unsupported driver: {value}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Production,
    NonProduction,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::NonProduction => "nonproduction",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "production" => Ok(Self::Production),
            "nonproduction" => Ok(Self::NonProduction),
            _ => Err(Error::InvalidInput(format!(
                "unknown source environment: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    Active,
    Archived,
}

impl SourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            _ => Err(Error::InvalidInput(format!(
                "unknown source status: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfilePurpose {
    Query,
    Mutate,
    Schema,
    Provision,
}

impl ProfilePurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutate => "mutate",
            Self::Schema => "schema",
            Self::Provision => "provision",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "query" => Ok(Self::Query),
            "mutate" => Ok(Self::Mutate),
            "schema" => Ok(Self::Schema),
            "provision" => Ok(Self::Provision),
            _ => Err(Error::InvalidInput(format!(
                "unknown profile purpose: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    Direct,
    Approval,
}

impl ApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Approval => "approval",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "direct" => Ok(Self::Direct),
            "approval" => Ok(Self::Approval),
            _ => Err(Error::InvalidInput(format!(
                "unknown approval policy: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    ListSources,
    ListTables,
    DescribeTable,
    ExplainQuery,
    QuerySql,
    MutateSql,
    ExecuteDdl,
    CreateDatabase,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListSources => "list_sources",
            Self::ListTables => "list_tables",
            Self::DescribeTable => "describe_table",
            Self::ExplainQuery => "explain_query",
            Self::QuerySql => "query_sql",
            Self::MutateSql => "mutate_sql",
            Self::ExecuteDdl => "execute_ddl",
            Self::CreateDatabase => "create_database",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "list_sources" => Ok(Self::ListSources),
            "list_tables" => Ok(Self::ListTables),
            "describe_table" => Ok(Self::DescribeTable),
            "explain_query" => Ok(Self::ExplainQuery),
            "query_sql" => Ok(Self::QuerySql),
            "mutate_sql" => Ok(Self::MutateSql),
            "execute_ddl" => Ok(Self::ExecuteDdl),
            "create_database" => Ok(Self::CreateDatabase),
            _ => Err(Error::InvalidInput(format!("unknown tool: {value}"))),
        }
    }

    pub fn purpose(self) -> Option<ProfilePurpose> {
        match self {
            Self::ListSources => None,
            Self::ListTables | Self::DescribeTable | Self::ExplainQuery | Self::QuerySql => {
                Some(ProfilePurpose::Query)
            }
            Self::MutateSql => Some(ProfilePurpose::Mutate),
            Self::ExecuteDdl => Some(ProfilePurpose::Schema),
            Self::CreateDatabase => Some(ProfilePurpose::Provision),
        }
    }

    pub fn is_write(self) -> bool {
        matches!(self, Self::MutateSql | Self::ExecuteDdl)
    }

    /// Stable presentation order for tool discovery.
    pub fn order(self) -> u8 {
        match self {
            Self::ListSources => 0,
            Self::ListTables => 1,
            Self::DescribeTable => 2,
            Self::ExplainQuery => 3,
            Self::QuerySql => 4,
            Self::MutateSql => 5,
            Self::ExecuteDdl => 6,
            Self::CreateDatabase => 7,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct IssuedToken {
    pub token: String,
    pub token_id: String,
    pub agent: String,
}

impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedToken")
            .field("token_id", &self.token_id)
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenIdentity {
    pub token_id: String,
    pub agent: String,
}

/// Administrative view of a token. The token hash and plaintext are never included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub id: String,
    pub agent: String,
    pub status: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// Administrative view of a long-lived admin key. Only the hash is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminKey {
    pub id: i64,
    pub label: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSource {
    pub id: String,
    pub driver: Driver,
    pub environment: Environment,
    pub parent_source_id: Option<String>,
    pub created_token_id: Option<String>,
    pub created_agent: Option<String>,
    pub created_origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: String,
    pub driver: Driver,
    pub environment: Environment,
    pub status: SourceStatus,
    pub parent_source_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProfile {
    pub source_id: String,
    pub name: String,
    pub purpose: ProfilePurpose,
    pub credential_ref: String,
    pub max_connections: Option<u32>,
    pub statement_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: i64,
    pub source_id: String,
    pub name: String,
    pub purpose: ProfilePurpose,
    pub credential_ref: String,
    pub enabled: bool,
    pub version: i64,
    pub max_connections: Option<u32>,
    pub statement_timeout_ms: Option<u64>,
}

/// Mutable profile fields. `purpose` is immutable; changing it requires a new
/// profile so existing approvals cannot silently gain different permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileUpdate {
    pub name: String,
    pub enabled: bool,
    /// `None` keeps the existing credential; `Some` rotates it.
    pub credential_ref: Option<String>,
    pub max_connections: Option<u32>,
    pub statement_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRoute {
    pub source_id: String,
    pub tool: Tool,
    pub profile_id: i64,
    pub min_approval_policy: ApprovalPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub id: i64,
    pub source_id: String,
    pub tool: Tool,
    pub profile_id: i64,
    pub min_approval_policy: ApprovalPolicy,
    pub version: i64,
}

/// Mutable route fields. `tool` is immutable; delete and recreate to change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteUpdate {
    pub profile_id: i64,
    pub min_approval_policy: ApprovalPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrant {
    pub token_id: String,
    pub source_id: String,
    pub tool: Tool,
    pub policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub token_id: String,
    pub source_id: String,
    pub tool: Tool,
    pub policy: ApprovalPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationStatus {
    Reserved,
    Committed,
    Released,
    Quarantined,
}

impl ReservationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Committed => "committed",
            Self::Released => "released",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "committed" => Ok(Self::Committed),
            "released" => Ok(Self::Released),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(Error::InvalidInput(format!(
                "unknown reservation status: {value}"
            ))),
        }
    }
}

/// One provisioning quota slot, reserved before any external database is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaReservation {
    pub id: i64,
    pub token_id: String,
    pub parent_source_id: String,
    pub database_name: String,
    pub approval_id: Option<i64>,
    pub status: ReservationStatus,
    pub reserved_at: i64,
    pub settled_at: Option<i64>,
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProvisionUsage {
    pub active: i64,
    pub recent: i64,
    pub quarantined: i64,
}

/// A route created together with a provisioned source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedRoute {
    pub tool: Tool,
    pub profile_name: String,
    pub min_approval_policy: ApprovalPolicy,
}

/// Everything registered atomically after a database was created successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedSource {
    pub source: NewSource,
    pub profiles: Vec<NewProfile>,
    pub routes: Vec<ProvisionedRoute>,
    pub grants: Vec<Tool>,
}

pub(crate) fn validate_source_id(value: &str) -> Result<()> {
    validate_identifier(value, 1, 64, "source id")
}

pub(crate) fn validate_profile_name(value: &str) -> Result<()> {
    validate_identifier(value, 1, 64, "profile name")
}

fn validate_identifier(value: &str, min: usize, max: usize, what: &str) -> Result<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(Error::InvalidInput(format!("{what} must not be empty")));
    };
    if !first.is_ascii_lowercase() {
        return Err(Error::InvalidInput(format!(
            "{what} must start with a lowercase ASCII letter"
        )));
    }
    if value.len() < min || value.len() > max {
        return Err(Error::InvalidInput(format!(
            "{what} must be {min}-{max} characters"
        )));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(Error::InvalidInput(format!(
            "{what} may contain only lowercase letters, digits, '_' and '-'"
        )));
    }
    Ok(())
}

pub(crate) fn validate_credential_ref(value: &str) -> Result<()> {
    let Some(account) = value.strip_prefix("keyring:") else {
        return Err(Error::InvalidInput(
            "credential_ref must use the keyring:<account> form".to_string(),
        ));
    };
    if account.is_empty()
        || account.len() > 128
        || !account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return Err(Error::InvalidInput(
            "credential_ref contains an invalid keyring account".to_string(),
        ));
    }
    Ok(())
}
