use std::fmt;

#[derive(Debug)]
pub enum Error {
    InvalidInput(String),
    Unauthenticated,
    TokenRevoked,
    SourceRequired,
    SourceNotGranted(String),
    SourceArchived(String),
    ToolNotGranted(String),
    ProductionWriteForbidden,
    RouteNotFound(String),
    ProfileUnavailable(i64),
    ProfileDisabled(i64),
    MetadataUnavailable,
    CredentialUnavailable,
    ResourceLimit(String),
    ProvisionBlocked(String),
    ProvisionUnavailable(String),
    ExecutionNotStarted(String),
    ExecutionUnknown,
    SqlRejected(String),
    NotFound(String),
    Conflict(String),
    ApprovalStale(&'static str),
    Database(rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::Unauthenticated => write!(f, "unauthenticated"),
            Self::TokenRevoked => write!(f, "token revoked"),
            Self::SourceRequired => {
                write!(f, "source is required when multiple sources are available")
            }
            Self::SourceNotGranted(source) => write!(f, "source is not granted: {source}"),
            Self::SourceArchived(source) => write!(f, "source is archived: {source}"),
            Self::ToolNotGranted(tool) => write!(f, "tool is not granted: {tool}"),
            Self::ProductionWriteForbidden => {
                write!(f, "production sources do not allow agent writes")
            }
            Self::RouteNotFound(route) => write!(f, "tool route is not configured: {route}"),
            Self::ProfileUnavailable(profile) => {
                write!(f, "connection profile is unavailable: {profile}")
            }
            Self::ProfileDisabled(profile) => {
                write!(f, "connection profile is disabled: {profile}")
            }
            Self::MetadataUnavailable => write!(f, "database metadata provider is unavailable"),
            Self::CredentialUnavailable => write!(f, "credential reference cannot be resolved"),
            Self::ResourceLimit(reason) => write!(f, "resource limit reached: {reason}"),
            Self::ProvisionBlocked(reason) => write!(f, "provisioning is blocked: {reason}"),
            Self::ProvisionUnavailable(reason) => {
                write!(f, "provisioning is unavailable: {reason}")
            }
            Self::ExecutionNotStarted(reason) => write!(f, "execution not started: {reason}"),
            Self::ExecutionUnknown => write!(f, "external execution result unknown"),
            Self::SqlRejected(reason) => write!(f, "SQL rejected: {reason}"),
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::Conflict(message) => write!(f, "conflict: {message}"),
            Self::ApprovalStale(reason) => {
                write!(f, "authorization changed before execution: {reason}")
            }
            Self::Database(error) => write!(f, "state database error: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(code, _) = &error {
            if code.code == rusqlite::ErrorCode::ConstraintViolation {
                return Self::Conflict("constraint violation".to_string());
            }
        }
        Self::Database(error)
    }
}
