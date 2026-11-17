use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::model::validate_source_id;
use crate::state::{Environment, Tool};

pub const DEFAULT_MAX_ACTIVE: u32 = 5;
pub const DEFAULT_MIN_INTERVAL_MS: i64 = 60_000;
pub const QUERY_RESULT_TOOLS: [Tool; 4] = [
    Tool::ListTables,
    Tool::DescribeTable,
    Tool::ExplainQuery,
    Tool::QuerySql,
];

const DATABASE_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
const SUFFIX_LEN: usize = 10;

/// Tools a provisioned source may receive as result grants. `create_database` and
/// `list_sources` are never inherited by a new source.
pub fn provisionable_tools() -> [Tool; 6] {
    [
        Tool::ListTables,
        Tool::DescribeTable,
        Tool::ExplainQuery,
        Tool::QuerySql,
        Tool::MutateSql,
        Tool::ExecuteDdl,
    ]
}

/// Admin-configured provisioning quota and result-grant template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionLimits {
    max_active: u32,
    min_interval_ms: i64,
    result_tools: Vec<Tool>,
}

impl ProvisionLimits {
    pub fn new(max_active: u32, min_interval_ms: i64, result_tools: Vec<Tool>) -> Result<Self> {
        if !(1..=100).contains(&max_active) {
            return Err(Error::InvalidInput(
                "provision max active databases must be between 1 and 100".to_string(),
            ));
        }
        if !(1..=86_400_000).contains(&min_interval_ms) {
            return Err(Error::InvalidInput(
                "provision interval must be between 1 and 86400000ms".to_string(),
            ));
        }
        if result_tools.is_empty() {
            return Err(Error::InvalidInput(
                "provision result tools must not be empty".to_string(),
            ));
        }
        let allowed = provisionable_tools();
        let mut normalized = Vec::new();
        for tool in allowed {
            if result_tools.contains(&tool) {
                normalized.push(tool);
            }
        }
        if result_tools.iter().any(|tool| !allowed.contains(tool)) {
            return Err(Error::InvalidInput(
                "provision result tools may only contain the query, mutate and DDL tools"
                    .to_string(),
            ));
        }
        Ok(Self {
            max_active,
            min_interval_ms,
            result_tools: normalized,
        })
    }

    pub fn max_active(&self) -> u32 {
        self.max_active
    }

    pub fn min_interval_ms(&self) -> i64 {
        self.min_interval_ms
    }

    /// Production sources never receive write tools, regardless of the template.
    pub fn tools_for(&self, environment: Environment) -> Vec<Tool> {
        match environment {
            Environment::Production => self
                .result_tools
                .iter()
                .copied()
                .filter(|tool| !tool.is_write())
                .collect(),
            Environment::NonProduction => self.result_tools.clone(),
        }
    }
}

impl Default for ProvisionLimits {
    fn default() -> Self {
        Self {
            max_active: DEFAULT_MAX_ACTIVE,
            min_interval_ms: DEFAULT_MIN_INTERVAL_MS,
            result_tools: QUERY_RESULT_TOOLS.to_vec(),
        }
    }
}

/// Server-generated provisioning request, persisted in the approval parameters and
/// therefore covered by the approval digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionSpec {
    pub database: String,
    pub environment: String,
    pub read_only_role: String,
    pub read_write_role: String,
    pub result_tools: Vec<String>,
}

impl ProvisionSpec {
    pub fn generate(
        environment: Environment,
        limits: &ProvisionLimits,
        label: Option<&str>,
    ) -> Result<Self> {
        let label = match label {
            Some(label) => format!("{}_", validate_label(label)?),
            None => String::new(),
        };
        let tag = match environment {
            Environment::Production => "prod",
            Environment::NonProduction => "np",
        };
        let database = format!("mcpdb_{tag}_{label}{}", random_suffix());
        let spec = Self {
            read_only_role: format!("{database}_ro"),
            read_write_role: format!("{database}_rw"),
            database,
            environment: environment.as_str().to_string(),
            result_tools: limits
                .tools_for(environment)
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<()> {
        validate_source_id(&self.database)?;
        self.environment()?;
        if self.read_only_role != format!("{}_ro", self.database)
            || self.read_write_role != format!("{}_rw", self.database)
        {
            return Err(Error::InvalidInput(
                "provision roles must be derived from the database name".to_string(),
            ));
        }
        if self.result_tools.is_empty() {
            return Err(Error::InvalidInput(
                "provision spec must grant at least one tool".to_string(),
            ));
        }
        self.tools()?;
        Ok(())
    }

    pub fn environment(&self) -> Result<Environment> {
        Environment::parse(&self.environment)
    }

    pub fn tools(&self) -> Result<Vec<Tool>> {
        let allowed = provisionable_tools();
        let mut tools = Vec::new();
        for value in &self.result_tools {
            let tool = Tool::parse(value)?;
            if !allowed.contains(&tool) {
                return Err(Error::InvalidInput(format!(
                    "provision result tool is not allowed: {value}"
                )));
            }
            tools.push(tool);
        }
        tools.sort_by_key(|tool| tool.order());
        tools.dedup();
        Ok(tools)
    }

    /// Canonical JSON: struct field order is fixed, so the approval digest is stable.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("provision spec is composed of serializable fields")
    }

    pub fn from_json(value: &str) -> Result<Self> {
        let spec: Self = serde_json::from_str(value)
            .map_err(|_| Error::InvalidInput("invalid provision spec".to_string()))?;
        spec.validate()?;
        Ok(spec)
    }
}

fn validate_label(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 12
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(Error::InvalidInput(
            "provision label must be 1-12 lowercase letters or digits".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn random_suffix() -> String {
    let mut bytes = [0_u8; SUFFIX_LEN];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|byte| DATABASE_ALPHABET[*byte as usize % DATABASE_ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_unsupported_tools_and_bounds() {
        assert!(ProvisionLimits::new(0, 60_000, QUERY_RESULT_TOOLS.to_vec()).is_err());
        assert!(ProvisionLimits::new(101, 60_000, QUERY_RESULT_TOOLS.to_vec()).is_err());
        assert!(ProvisionLimits::new(5, 0, QUERY_RESULT_TOOLS.to_vec()).is_err());
        assert!(ProvisionLimits::new(5, 86_400_001, QUERY_RESULT_TOOLS.to_vec()).is_err());
        assert!(ProvisionLimits::new(5, 60_000, Vec::new()).is_err());
        assert!(ProvisionLimits::new(5, 60_000, vec![Tool::CreateDatabase]).is_err());
        assert!(ProvisionLimits::new(5, 60_000, vec![Tool::ListSources]).is_err());
    }

    #[test]
    fn template_is_normalized_and_production_is_read_only() {
        let limits = ProvisionLimits::new(
            2,
            1_000,
            vec![
                Tool::MutateSql,
                Tool::QuerySql,
                Tool::QuerySql,
                Tool::ListTables,
            ],
        )
        .unwrap();
        assert_eq!(
            limits.tools_for(Environment::NonProduction),
            vec![Tool::ListTables, Tool::QuerySql, Tool::MutateSql]
        );
        assert_eq!(
            limits.tools_for(Environment::Production),
            vec![Tool::ListTables, Tool::QuerySql]
        );
    }

    #[test]
    fn generated_specs_are_unique_valid_and_stable() {
        let limits = ProvisionLimits::default();
        let first =
            ProvisionSpec::generate(Environment::NonProduction, &limits, Some("scratch")).unwrap();
        let second =
            ProvisionSpec::generate(Environment::NonProduction, &limits, Some("scratch")).unwrap();
        assert_ne!(first.database, second.database);
        assert!(first.database.starts_with("mcpdb_np_scratch_"));
        assert_eq!(first.read_only_role, format!("{}_ro", first.database));
        assert_eq!(ProvisionSpec::from_json(&first.to_json()).unwrap(), first);
        assert_eq!(
            first.to_json(),
            ProvisionSpec::from_json(&first.to_json())
                .unwrap()
                .to_json()
        );
    }

    #[test]
    fn spec_rejects_tampered_roles_tools_and_environment() {
        let limits = ProvisionLimits::default();
        let mut spec = ProvisionSpec::generate(Environment::NonProduction, &limits, None).unwrap();
        let mut tampered = spec.clone();
        tampered.read_write_role = "other_rw".to_string();
        assert!(tampered.validate().is_err());
        let mut tampered = spec.clone();
        tampered.result_tools = vec!["create_database".to_string()];
        assert!(tampered.validate().is_err());
        let mut tampered = spec.clone();
        tampered.environment = "testing".to_string();
        assert!(tampered.validate().is_err());
        spec.result_tools = vec!["query_sql".to_string(), "list_tables".to_string()];
        assert_eq!(
            spec.tools().unwrap(),
            vec![Tool::ListTables, Tool::QuerySql]
        );
        assert!(ProvisionSpec::from_json("{\"database\":\"x\"}").is_err());
    }
}
