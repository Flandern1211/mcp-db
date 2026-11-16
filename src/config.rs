use std::env;
use std::fmt;
use std::path::PathBuf;

use crate::DEFAULT_ADDR;

pub const DEFAULT_STATE_PATH: &str = ".mcp-db/state.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub addr: String,
    pub state_path: PathBuf,
}

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

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

        Ok(Self { addr, state_path })
    }
}
