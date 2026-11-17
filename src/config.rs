use std::env;
use std::fmt;
use std::path::PathBuf;

use crate::DEFAULT_ADDR;

pub const DEFAULT_STATE_PATH: &str = ".mcp-db/state.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub addr: String,
    pub state_path: PathBuf,
    pub credential_store: Option<PathBuf>,
    pub master_key_file: Option<PathBuf>,
    pub approval_timeout_ms: i64,
    pub approval_execution_ttl_ms: i64,
    pub token_requests_per_minute: usize,
}

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn parse_token_rate(value: Option<&str>) -> Result<usize, ConfigError> {
    match value {
        None => Ok(crate::limits::DEFAULT_TOKEN_RATE_LIMIT),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=100_000).contains(value))
            .ok_or_else(|| {
                ConfigError(
                    "MCPDB_TOKEN_REQUESTS_PER_MINUTE must be between 1 and 100000".to_string(),
                )
            }),
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let addr = env::var("MCPDB_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
        if addr.trim().is_empty() {
            return Err(ConfigError("MCPDB_ADDR must not be empty".to_string()));
        }

        let state_path = env::var_os("MCPDB_STATE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_PATH));
        if state_path.as_os_str().is_empty() {
            return Err(ConfigError(
                "MCPDB_STATE_PATH must not be empty".to_string(),
            ));
        }

        let credential_store = env::var_os("MCPDB_CREDENTIAL_STORE").map(PathBuf::from);
        let master_key_file = env::var_os("MCPDB_MASTER_KEY_FILE").map(PathBuf::from);
        if credential_store.is_some() != master_key_file.is_some() {
            return Err(ConfigError(
                "MCPDB_CREDENTIAL_STORE and MCPDB_MASTER_KEY_FILE must be set together".to_string(),
            ));
        }
        if credential_store
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
            || master_key_file
                .as_ref()
                .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError(
                "credential paths must not be empty".to_string(),
            ));
        }

        Ok(Self {
            addr,
            state_path,
            credential_store,
            master_key_file,
            approval_timeout_ms: 120_000,
            approval_execution_ttl_ms: 60_000,
            token_requests_per_minute: parse_token_rate(
                env::var("MCPDB_TOKEN_REQUESTS_PER_MINUTE").ok().as_deref(),
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_rate_has_a_bounded_positive_configuration() {
        assert_eq!(parse_token_rate(None).unwrap(), 60);
        assert_eq!(parse_token_rate(Some("10")).unwrap(), 10);
        for invalid in ["", "0", "-1", "invalid", "100001"] {
            assert!(parse_token_rate(Some(invalid)).is_err());
        }
    }
}
