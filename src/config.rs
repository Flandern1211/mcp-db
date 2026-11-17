use std::env;
use std::fmt;
use std::path::PathBuf;

use crate::provision::{ProvisionLimits, QUERY_RESULT_TOOLS};
use crate::state::Tool;
use crate::DEFAULT_ADDR;

pub const DEFAULT_STATE_PATH: &str = ".mcp-db/state.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub addr: String,
    /// Separate listener for the local admin UI so the MCP port stays untouched.
    pub admin_addr: String,
    pub state_path: PathBuf,
    pub credential_store: Option<PathBuf>,
    pub master_key_file: Option<PathBuf>,
    pub approval_timeout_ms: i64,
    pub approval_execution_ttl_ms: i64,
    pub token_requests_per_minute: usize,
    pub provision_limits: ProvisionLimits,
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

fn parse_provision_limits(
    max_active: Option<&str>,
    min_interval: Option<&str>,
    result_tools: Option<&str>,
) -> Result<ProvisionLimits, ConfigError> {
    let max_active = match max_active {
        None => crate::provision::DEFAULT_MAX_ACTIVE,
        Some(value) => value
            .parse::<u32>()
            .ok()
            .filter(|value| (1..=100).contains(value))
            .ok_or_else(|| {
                ConfigError("MCPDB_PROVISION_MAX_ACTIVE must be between 1 and 100".to_string())
            })?,
    };
    let min_interval_ms = match min_interval {
        None => crate::provision::DEFAULT_MIN_INTERVAL_MS,
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|value| (1..=86_400_000).contains(value))
            .ok_or_else(|| {
                ConfigError(
                    "MCPDB_PROVISION_MIN_INTERVAL_MS must be between 1 and 86400000".to_string(),
                )
            })?,
    };
    let result_tools = match result_tools {
        None => QUERY_RESULT_TOOLS.to_vec(),
        Some(value) => {
            let mut tools = Vec::new();
            for name in value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                let tool = Tool::parse(name).map_err(|_| {
                    ConfigError(format!(
                        "MCPDB_PROVISION_RESULT_TOOLS contains an unknown tool: {name}"
                    ))
                })?;
                if !tools.contains(&tool) {
                    tools.push(tool);
                }
            }
            if tools.is_empty() {
                return Err(ConfigError(
                    "MCPDB_PROVISION_RESULT_TOOLS must list at least one tool".to_string(),
                ));
            }
            tools
        }
    };
    ProvisionLimits::new(max_active, min_interval_ms, result_tools)
        .map_err(|error| ConfigError(format!("invalid provisioning limits: {error}")))
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let addr = env::var("MCPDB_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
        if addr.trim().is_empty() {
            return Err(ConfigError("MCPDB_ADDR must not be empty".to_string()));
        }

        let admin_addr =
            env::var("MCPDB_ADMIN_ADDR").unwrap_or_else(|_| crate::ADMIN_ADDR.to_string());
        if admin_addr.trim().is_empty() {
            return Err(ConfigError(
                "MCPDB_ADMIN_ADDR must not be empty".to_string(),
            ));
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
            admin_addr,
            state_path,
            credential_store,
            master_key_file,
            approval_timeout_ms: 120_000,
            approval_execution_ttl_ms: 60_000,
            token_requests_per_minute: parse_token_rate(
                env::var("MCPDB_TOKEN_REQUESTS_PER_MINUTE").ok().as_deref(),
            )?,
            provision_limits: parse_provision_limits(
                env::var("MCPDB_PROVISION_MAX_ACTIVE").ok().as_deref(),
                env::var("MCPDB_PROVISION_MIN_INTERVAL_MS").ok().as_deref(),
                env::var("MCPDB_PROVISION_RESULT_TOOLS").ok().as_deref(),
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

    #[test]
    fn provisioning_limits_parse_from_environment_values() {
        let defaults = parse_provision_limits(None, None, None).unwrap();
        assert_eq!(defaults, ProvisionLimits::default());
        let configured =
            parse_provision_limits(Some("2"), Some("90000"), Some("query_sql,list_tables"))
                .unwrap();
        assert_eq!(configured.max_active(), 2);
        assert_eq!(configured.min_interval_ms(), 90_000);
        assert_eq!(
            configured.tools_for(crate::state::Environment::NonProduction),
            vec![Tool::ListTables, Tool::QuerySql]
        );
        assert!(parse_provision_limits(Some("0"), None, None).is_err());
        assert!(parse_provision_limits(Some("101"), None, None).is_err());
        assert!(parse_provision_limits(None, Some("0"), None).is_err());
        assert!(parse_provision_limits(None, None, Some("")).is_err());
        assert!(parse_provision_limits(None, None, Some("create_database")).is_err());
        assert!(parse_provision_limits(None, None, Some("not_a_tool")).is_err());
    }
}
