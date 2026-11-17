//! Legacy Bun/TypeScript v1 migration: snapshot, staged import, journaled cutover
//! and recovery. The journal is the only source of truth for an interrupted run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::approval::AuditEvent;
use crate::auth;
use crate::credentials::{CredentialResolver, CredentialSink, EncryptedFileResolver};
use crate::error::{Error, Result};
use crate::state::model::{validate_credential_ref, validate_source_id};
use crate::state::{
    ApprovalPolicy, Driver, Environment, NewGrant, NewProfile, NewRoute, NewSource, ProfilePurpose,
    SourceStatus, Store, Tool,
};

pub const STAGE_SNAPSHOT: &str = "snapshot";
pub const STAGE_STAGING_READY: &str = "staging_ready";
pub const STAGE_STATE_COMMITTED: &str = "state_committed";
pub const STAGE_DONE: &str = "done";
pub const STAGE_FAILED_PRECOMMIT: &str = "failed_precommit";

const INJECTED_FAILURE: &str = "injected migration failure (test hook)";
const READ_TOOLS: [Tool; 4] = [
    Tool::ListTables,
    Tool::DescribeTable,
    Tool::ExplainQuery,
    Tool::QuerySql,
];
const WRITE_TOOLS: [Tool; 2] = [Tool::MutateSql, Tool::ExecuteDdl];

/// Test-only crash hook. The CLI never sets this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    BeforeCommit,
    AfterStateCommit,
}

#[derive(Debug, Clone)]
pub struct MigrateOptions {
    pub legacy_config: PathBuf,
    pub environment: Environment,
    pub state_path: PathBuf,
    pub credential_store: PathBuf,
    pub master_key_file: PathBuf,
    pub fail_after: Option<FailPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub sources: Vec<String>,
    pub agents: Vec<String>,
    pub warnings: Vec<String>,
    pub snapshot_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    RolledBack,
    Completed,
    NothingToDo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationJournal {
    pub stage: String,
    pub started_at: i64,
    pub updated_at: i64,
    pub state_path: PathBuf,
    pub config_path: PathBuf,
    pub snapshot_dir: PathBuf,
    pub snapshot_config: PathBuf,
    pub snapshot_state: PathBuf,
    pub staging_state: PathBuf,
}

impl MigrationJournal {
    fn set_stage(&mut self, stage: &str) {
        self.stage = stage.to_string();
        self.updated_at = now_ms();
    }
}

#[derive(Deserialize)]
struct LegacyFile {
    server: toml::Value,
    #[serde(default)]
    sources: Vec<LegacySource>,
    #[serde(default)]
    agents: BTreeMap<String, LegacyAgent>,
}

#[derive(Deserialize)]
struct LegacySource {
    id: String,
    driver: String,
    dsn_readonly: String,
    dsn_readwrite: String,
    #[serde(default)]
    max_rows: Option<i64>,
    #[serde(default)]
    statement_timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct LegacyAgent {
    token_env: String,
    #[serde(default)]
    sources: BTreeMap<String, String>,
}

struct SourcePlan {
    id: String,
    dsn_readonly: String,
    dsn_readwrite: String,
    max_rows: Option<i64>,
    statement_timeout_ms: Option<u64>,
}

struct AgentPlan {
    id: String,
    token: String,
    modes: BTreeMap<String, String>,
}

struct MigrationPlan {
    legacy_state: PathBuf,
    sources: Vec<SourcePlan>,
    agents: Vec<AgentPlan>,
}

impl std::fmt::Debug for MigrationPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MigrationPlan")
            .field(
                "sources",
                &self
                    .sources
                    .iter()
                    .map(|source| &source.id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "agents",
                &self
                    .agents
                    .iter()
                    .map(|agent| &agent.id)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

pub fn journal_path(state_path: &Path) -> PathBuf {
    let state_path = absolute(state_path);
    state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("legacy-migration")
        .join("journal.json")
}

/// Refuses to start MCP/admin service traffic while a migration cutover is
/// incomplete. A completed rollback (`failed_precommit`) leaves the pre-migration
/// state intact and is allowed to start.
pub fn guard_startup(state_path: &Path) -> Result<()> {
    match read_journal(state_path)? {
        None => Ok(()),
        Some(journal) if journal.stage == STAGE_DONE || journal.stage == STAGE_FAILED_PRECOMMIT => {
            Ok(())
        }
        Some(journal) => Err(Error::InvalidInput(format!(
            "legacy migration is incomplete (stage {}); run mcp-db recover-legacy-migration",
            journal.stage
        ))),
    }
}

pub fn run_migration(options: &MigrateOptions) -> Result<MigrationReport> {
    let config_path = absolute(&options.legacy_config);
    let state_path = absolute(&options.state_path);
    if options.credential_store.as_os_str().is_empty()
        || options.master_key_file.as_os_str().is_empty()
    {
        return Err(Error::InvalidInput(
            "MCPDB_CREDENTIAL_STORE and MCPDB_MASTER_KEY_FILE must be configured".to_string(),
        ));
    }
    if let Some(existing) = read_journal(&state_path)? {
        return Err(Error::Conflict(format!(
            "a legacy migration journal already exists (stage {}); refusing to overwrite",
            existing.stage
        )));
    }

    // Everything below is validated before any target file is written.
    let plan = plan_migration(&config_path)?;
    ensure_production_has_no_write_grants(options.environment, &plan)?;
    ensure_no_stale_wal(&state_path, &plan.legacy_state)?;

    let state_dir = state_path
        .parent()
        .ok_or_else(|| Error::InvalidInput("state path has no parent directory".to_string()))?
        .to_path_buf();
    fs::create_dir_all(&state_dir).map_err(io_error)?;
    let snapshot_dir = create_snapshot_dir(&state_dir)?;
    let snapshot_config = snapshot_dir.join("legacy-config.toml");
    let snapshot_state = snapshot_dir.join("legacy-state.db");
    copy_file_secure(&config_path, &snapshot_config)?;
    copy_sqlite_set(&plan.legacy_state, &snapshot_state)?;

    let staging_state = state_dir.join("legacy-migration").join("staging.db");
    let mut journal = MigrationJournal {
        stage: STAGE_SNAPSHOT.to_string(),
        started_at: now_ms(),
        updated_at: now_ms(),
        state_path: state_path.clone(),
        config_path: config_path.clone(),
        snapshot_dir: snapshot_dir.clone(),
        snapshot_config: snapshot_config.clone(),
        snapshot_state: snapshot_state.clone(),
        staging_state: staging_state.clone(),
    };
    write_journal(&journal)?;

    let result = execute_migration(
        options,
        &plan,
        &mut journal,
        &config_path,
        &state_path,
        &snapshot_config,
        &staging_state,
    );
    match result {
        Ok(report) => Ok(report),
        Err(error) => {
            if !is_injected(&error)
                && matches!(journal.stage.as_str(), STAGE_SNAPSHOT | STAGE_STAGING_READY)
            {
                let _ = remove_sqlite_set(&journal.staging_state);
                journal.set_stage(STAGE_FAILED_PRECOMMIT);
                let _ = write_journal(&journal);
                let _ = audit_active(
                    &state_path,
                    "legacy_migration_rolled_back",
                    "rejected",
                    "LEGACY_MIGRATION_ROLLED_BACK",
                );
            }
            Err(error)
        }
    }
}

pub fn recover_legacy_migration(state_path: &Path) -> Result<RecoveryOutcome> {
    let state_path = absolute(state_path);
    let Some(journal) = read_journal(&state_path)? else {
        return Err(Error::NotFound("legacy migration journal".to_string()));
    };
    match journal.stage.as_str() {
        STAGE_DONE | STAGE_FAILED_PRECOMMIT => Ok(RecoveryOutcome::NothingToDo),
        STAGE_SNAPSHOT => {
            rollback_precommit(&journal)?;
            Ok(RecoveryOutcome::RolledBack)
        }
        STAGE_STAGING_READY => {
            // A missing staging file after `staging_ready` means the state rename
            // completed before the journal update; finish forward, never roll back.
            if journal.staging_state.exists() {
                rollback_precommit(&journal)?;
                Ok(RecoveryOutcome::RolledBack)
            } else {
                complete_committed(&journal)?;
                Ok(RecoveryOutcome::Completed)
            }
        }
        STAGE_STATE_COMMITTED => {
            complete_committed(&journal)?;
            Ok(RecoveryOutcome::Completed)
        }
        other => Err(Error::InvalidInput(format!(
            "unknown migration journal stage: {other}"
        ))),
    }
}

fn execute_migration(
    options: &MigrateOptions,
    plan: &MigrationPlan,
    journal: &mut MigrationJournal,
    config_path: &Path,
    state_path: &Path,
    snapshot_config: &Path,
    staging_state: &Path,
) -> Result<MigrationReport> {
    let resolver =
        EncryptedFileResolver::open_or_create(&options.credential_store, &options.master_key_file)?;
    store_credentials(&resolver, plan)?;

    remove_sqlite_set(staging_state)?;
    copy_sqlite_set(&plan.legacy_state, staging_state)?;

    let store = Store::open_admin(staging_state)?;
    let token_ids = import_staging(&store, plan, options.environment)?;
    validate_staging(&store, plan, options, &token_ids, &resolver)?;
    store.checkpoint()?;
    drop(store);

    remove_sqlite_sidecars(staging_state)?;
    sync_file(staging_state)?;
    journal.set_stage(STAGE_STAGING_READY);
    write_journal(journal)?;
    injected(options, FailPoint::BeforeCommit)?;

    remove_sqlite_sidecars(state_path)?;
    replace_file(staging_state, state_path)?;
    journal.set_stage(STAGE_STATE_COMMITTED);
    write_journal(journal)?;
    injected(options, FailPoint::AfterStateCommit)?;

    write_sanitized_config(snapshot_config, config_path)?;
    audit_active(
        state_path,
        "legacy_migration_completed",
        "allowed",
        "LEGACY_MIGRATION_COMPLETED",
    )?;
    journal.set_stage(STAGE_DONE);
    write_journal(journal)?;

    let warnings = plan
        .sources
        .iter()
        .map(|source| {
            format!(
                "source {}: legacy max_rows={} is not migrated; global result limits apply",
                source.id,
                source.max_rows.unwrap_or(500)
            )
        })
        .collect();
    Ok(MigrationReport {
        sources: plan
            .sources
            .iter()
            .map(|source| source.id.clone())
            .collect(),
        agents: plan.agents.iter().map(|agent| agent.id.clone()).collect(),
        warnings,
        snapshot_dir: journal.snapshot_dir.clone(),
    })
}

fn plan_migration(config_path: &Path) -> Result<MigrationPlan> {
    let text = fs::read_to_string(config_path).map_err(io_error)?;
    let file: LegacyFile = toml::from_str(&text)
        .map_err(|error| Error::InvalidInput(format!("cannot parse legacy config: {error}")))?;
    let server_state_path = file
        .server
        .get("state_path")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::InvalidInput("legacy config is missing server.state_path".to_string())
        })?;
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let legacy_state = absolute(&config_dir.join(server_state_path));
    if !legacy_state.is_file() {
        return Err(Error::InvalidInput(format!(
            "legacy state database does not exist: {}",
            legacy_state.display()
        )));
    }

    let mut source_ids = BTreeSet::new();
    let mut sources = Vec::with_capacity(file.sources.len());
    for source in file.sources {
        validate_source_id(&source.id)?;
        if source.driver != "postgres" {
            return Err(Error::InvalidInput(format!(
                "source {} uses unsupported driver {}; only postgres can be migrated",
                source.id, source.driver
            )));
        }
        if source.dsn_readonly.trim().is_empty() || source.dsn_readwrite.trim().is_empty() {
            return Err(Error::InvalidInput(format!(
                "source {} is missing a DSN",
                source.id
            )));
        }
        if !source_ids.insert(source.id.clone()) {
            return Err(Error::InvalidInput(format!(
                "duplicate legacy source id: {}",
                source.id
            )));
        }
        sources.push(SourcePlan {
            id: source.id,
            dsn_readonly: source.dsn_readonly,
            dsn_readwrite: source.dsn_readwrite,
            max_rows: source.max_rows,
            statement_timeout_ms: source.statement_timeout_ms,
        });
    }

    let mut agents = Vec::with_capacity(file.agents.len());
    let mut hashes = BTreeSet::new();
    for (id, agent) in file.agents {
        let token_env = agent.token_env.trim().to_string();
        if token_env.is_empty() {
            return Err(Error::InvalidInput(format!(
                "agent {id} is missing token_env"
            )));
        }
        let token = std::env::var(&token_env).map_err(|_| {
            Error::InvalidInput(format!(
                "agent {id} token environment variable {token_env} is not set"
            ))
        })?;
        if token.trim().is_empty() {
            return Err(Error::InvalidInput(format!(
                "agent {id} token environment variable {token_env} is empty"
            )));
        }
        let hash = auth::hash_token(&token);
        if !hashes.insert(hash) {
            return Err(Error::InvalidInput(
                "two legacy agents use the same token".to_string(),
            ));
        }
        for (source_id, mode) in &agent.sources {
            if !source_ids.contains(source_id) {
                return Err(Error::InvalidInput(format!(
                    "agent {id} references unknown source {source_id}"
                )));
            }
            tools_for_mode(mode).map_err(|_| {
                Error::InvalidInput(format!(
                    "agent {id} has unsupported mode {mode} on source {source_id}"
                ))
            })?;
        }
        agents.push(AgentPlan {
            id,
            token,
            modes: agent.sources,
        });
    }

    Ok(MigrationPlan {
        legacy_state,
        sources,
        agents,
    })
}

fn ensure_production_has_no_write_grants(
    environment: Environment,
    plan: &MigrationPlan,
) -> Result<()> {
    if environment != Environment::Production {
        return Ok(());
    }
    for agent in &plan.agents {
        for (source_id, mode) in &agent.modes {
            if matches!(mode.as_str(), "readwrite" | "approval") {
                return Err(Error::InvalidInput(format!(
                    "cannot migrate agent {} into production source {}: mode {} generates write grants; use --environment nonproduction or remove write modes first",
                    agent.id, source_id, mode
                )));
            }
        }
    }
    Ok(())
}

fn ensure_no_stale_wal(state_path: &Path, legacy_state: &Path) -> Result<()> {
    for path in [state_path, legacy_state] {
        let wal = sidecar(path, "-wal");
        match fs::metadata(&wal) {
            Ok(metadata) if metadata.len() > 0 => {
                return Err(Error::InvalidInput(format!(
                    "state database WAL file is not empty: {}; stop the service and checkpoint/close it normally before migrating",
                    wal.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

fn import_staging(
    store: &Store,
    plan: &MigrationPlan,
    environment: Environment,
) -> Result<BTreeMap<String, String>> {
    let now = now_ms();
    store.record_audit_event(&migration_event(
        "legacy_migration_started",
        "allowed",
        "LEGACY_MIGRATION_STARTED",
        now,
    ))?;

    let mut token_ids = BTreeMap::new();
    for agent in &plan.agents {
        let hash = auth::hash_token(&agent.token);
        let token_id = store.import_token(&agent.id, &hash)?;
        token_ids.insert(agent.id.clone(), token_id);
    }

    for source in &plan.sources {
        store.create_source(&NewSource {
            id: source.id.clone(),
            driver: Driver::Postgres,
            // Grants are staged while the source is non-production and archived
            // immediately afterwards; the requested environment is applied only then.
            environment: Environment::NonProduction,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: Some("legacy-migration".to_string()),
        })?;
        let query_ref = format!("keyring:legacy/{}/query", source.id);
        let mutate_ref = format!("keyring:legacy/{}/mutate", source.id);
        let schema_ref = format!("keyring:legacy/{}/schema", source.id);
        let query = store.create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "query".to_string(),
            purpose: ProfilePurpose::Query,
            credential_ref: query_ref,
            max_connections: None,
            statement_timeout_ms: source.statement_timeout_ms,
        })?;
        let mutate = store.create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "mutate".to_string(),
            purpose: ProfilePurpose::Mutate,
            credential_ref: mutate_ref,
            max_connections: None,
            statement_timeout_ms: source.statement_timeout_ms,
        })?;
        let schema = store.create_profile(&NewProfile {
            source_id: source.id.clone(),
            name: "schema".to_string(),
            purpose: ProfilePurpose::Schema,
            credential_ref: schema_ref,
            max_connections: None,
            statement_timeout_ms: source.statement_timeout_ms,
        })?;
        for tool in READ_TOOLS {
            store.create_route(&NewRoute {
                source_id: source.id.clone(),
                tool,
                profile_id: query.id,
                min_approval_policy: ApprovalPolicy::Direct,
            })?;
        }
        store.create_route(&NewRoute {
            source_id: source.id.clone(),
            tool: Tool::MutateSql,
            profile_id: mutate.id,
            min_approval_policy: ApprovalPolicy::Direct,
        })?;
        store.create_route(&NewRoute {
            source_id: source.id.clone(),
            tool: Tool::ExecuteDdl,
            profile_id: schema.id,
            min_approval_policy: ApprovalPolicy::Direct,
        })?;
        for agent in &plan.agents {
            let Some(mode) = agent.modes.get(&source.id) else {
                continue;
            };
            let token_id = token_ids
                .get(&agent.id)
                .ok_or_else(|| Error::NotFound(format!("token for agent {}", agent.id)))?;
            for tool in tools_for_mode(mode)? {
                store.create_grant(&NewGrant {
                    token_id: token_id.clone(),
                    source_id: source.id.clone(),
                    tool,
                    policy: Some(policy_for_mode(mode, tool)),
                })?;
            }
        }
        // Legacy sources are disabled after migration; an admin must classify and
        // restore them explicitly.
        store.archive_source(&source.id, "admin-cli", now)?;
        store.set_source_environment(&source.id, environment)?;
    }
    Ok(token_ids)
}

fn validate_staging(
    store: &Store,
    plan: &MigrationPlan,
    options: &MigrateOptions,
    token_ids: &BTreeMap<String, String>,
    resolver: &EncryptedFileResolver,
) -> Result<()> {
    let sources = store.list_all_sources()?;
    let actual_ids = sources
        .iter()
        .map(|source| source.id.clone())
        .collect::<BTreeSet<_>>();
    let expected_ids = plan
        .sources
        .iter()
        .map(|source| source.id.clone())
        .collect::<BTreeSet<_>>();
    if actual_ids != expected_ids {
        return Err(Error::InvalidInput(
            "staged sources do not match the legacy config".to_string(),
        ));
    }

    for source in &sources {
        if source.status != SourceStatus::Archived {
            return Err(Error::InvalidInput(format!(
                "migrated source {} must stay archived",
                source.id
            )));
        }
        if source.environment != options.environment {
            return Err(Error::InvalidInput(format!(
                "migrated source {} has the wrong environment",
                source.id
            )));
        }
        let plan_source = plan
            .sources
            .iter()
            .find(|candidate| candidate.id == source.id)
            .ok_or_else(|| Error::NotFound(format!("source plan {}", source.id)))?;
        let profiles = store.list_profiles(&source.id)?;
        if profiles.len() != 3 {
            return Err(Error::InvalidInput(format!(
                "migrated source {} must have exactly three profiles",
                source.id
            )));
        }
        let mut purposes = BTreeSet::new();
        for profile in &profiles {
            let expected_ref = match profile.purpose {
                ProfilePurpose::Query => format!("keyring:legacy/{}/query", source.id),
                ProfilePurpose::Mutate => format!("keyring:legacy/{}/mutate", source.id),
                ProfilePurpose::Schema => format!("keyring:legacy/{}/schema", source.id),
                ProfilePurpose::Provision => {
                    return Err(Error::InvalidInput(format!(
                        "migrated source {} must not have a provision profile",
                        source.id
                    )))
                }
            };
            validate_credential_ref(&profile.credential_ref)?;
            if profile.credential_ref != expected_ref {
                return Err(Error::InvalidInput(format!(
                    "profile {} has an unexpected credential reference",
                    profile.id
                )));
            }
            if !profile.enabled {
                return Err(Error::InvalidInput(format!(
                    "profile {} must be enabled",
                    profile.id
                )));
            }
            if profile.statement_timeout_ms != plan_source.statement_timeout_ms {
                return Err(Error::InvalidInput(format!(
                    "profile {} lost the legacy statement timeout",
                    profile.id
                )));
            }
            resolver.resolve(&profile.credential_ref)?;
            purposes.insert(profile.purpose.as_str().to_string());
        }
        if purposes.len() != 3 {
            return Err(Error::InvalidInput(format!(
                "source {} profiles do not cover query, mutate and schema",
                source.id
            )));
        }
        for (tool, purpose) in READ_TOOLS
            .iter()
            .map(|tool| (*tool, ProfilePurpose::Query))
            .chain([
                (Tool::MutateSql, ProfilePurpose::Mutate),
                (Tool::ExecuteDdl, ProfilePurpose::Schema),
            ])
        {
            let route = store.get_route(&source.id, tool)?;
            if route.min_approval_policy != ApprovalPolicy::Direct {
                return Err(Error::InvalidInput(format!(
                    "route {}/{} must use direct as its minimum policy",
                    source.id,
                    tool.as_str()
                )));
            }
            let profile = store.get_profile(route.profile_id)?;
            if profile.purpose != purpose {
                return Err(Error::InvalidInput(format!(
                    "route {}/{} points at the wrong profile purpose",
                    source.id,
                    tool.as_str()
                )));
            }
        }
        if store.get_route(&source.id, Tool::CreateDatabase).is_ok() {
            return Err(Error::InvalidInput(format!(
                "source {} must not have a create_database route",
                source.id
            )));
        }
    }

    for agent in &plan.agents {
        let token_id = token_ids
            .get(&agent.id)
            .ok_or_else(|| Error::NotFound(format!("token for agent {}", agent.id)))?;
        store.authenticate(&agent.token)?;
        let actual = grant_signature(
            store
                .list_grants_for_token(token_id)?
                .into_iter()
                .map(|grant| (grant.source_id, grant.tool, grant.policy)),
        );
        let mut expected = Vec::new();
        for (source_id, mode) in &agent.modes {
            for tool in tools_for_mode(mode)? {
                expected.push((source_id.clone(), tool, policy_for_mode(mode, tool)));
            }
        }
        let expected = grant_signature(expected);
        if actual != expected {
            return Err(Error::InvalidInput(format!(
                "migrated grants for agent {} do not match the fixed matrix",
                agent.id
            )));
        }
        for grant in store.list_grants_for_token(token_id)? {
            if grant.tool == Tool::CreateDatabase {
                return Err(Error::InvalidInput(
                    "legacy migration must never grant create_database".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn grant_signature(
    grants: impl IntoIterator<Item = (String, Tool, ApprovalPolicy)>,
) -> Vec<(String, u8, u8)> {
    let mut grants = grants
        .into_iter()
        .map(|(source_id, tool, policy)| {
            (
                source_id,
                tool.order(),
                u8::from(policy == ApprovalPolicy::Approval),
            )
        })
        .collect::<Vec<_>>();
    grants.sort();
    grants
}

fn store_credentials(resolver: &EncryptedFileResolver, plan: &MigrationPlan) -> Result<()> {
    for source in &plan.sources {
        for (purpose, dsn) in [
            ("query", source.dsn_readonly.as_str()),
            ("mutate", source.dsn_readwrite.as_str()),
            ("schema", source.dsn_readwrite.as_str()),
        ] {
            let reference = format!("keyring:legacy/{}/{}", source.id, purpose);
            validate_credential_ref(&reference)?;
            resolver.store(&reference, dsn)?;
        }
    }
    Ok(())
}

fn tools_for_mode(mode: &str) -> Result<Vec<Tool>> {
    let mut tools = READ_TOOLS.to_vec();
    match mode {
        "readonly" => {}
        "readwrite" | "approval" => tools.extend(WRITE_TOOLS),
        _ => return Err(Error::InvalidInput(format!("unknown mode: {mode}"))),
    }
    Ok(tools)
}

fn policy_for_mode(mode: &str, tool: Tool) -> ApprovalPolicy {
    if mode == "approval" && tool.is_write() {
        ApprovalPolicy::Approval
    } else {
        ApprovalPolicy::Direct
    }
}

fn rollback_precommit(journal: &MigrationJournal) -> Result<()> {
    remove_sqlite_set(&journal.staging_state)?;
    let mut journal = journal.clone();
    journal.set_stage(STAGE_FAILED_PRECOMMIT);
    write_journal(&journal)?;
    let _ = audit_active(
        &journal.state_path,
        "legacy_migration_rolled_back",
        "rejected",
        "LEGACY_MIGRATION_ROLLED_BACK",
    );
    Ok(())
}

fn complete_committed(journal: &MigrationJournal) -> Result<()> {
    if journal.staging_state.exists() {
        // Journal recorded the commit point but the process stopped before the
        // rename; the validated staging file is still the source of truth.
        remove_sqlite_sidecars(&journal.state_path)?;
        replace_file(&journal.staging_state, &journal.state_path)?;
    }
    if !journal.state_path.exists() {
        return Err(Error::Conflict(
            "committed migration has no active state database".to_string(),
        ));
    }
    write_sanitized_config(&journal.snapshot_config, &journal.config_path)?;
    audit_active(
        &journal.state_path,
        "legacy_migration_completed",
        "allowed",
        "LEGACY_MIGRATION_COMPLETED",
    )?;
    let mut journal = journal.clone();
    journal.set_stage(STAGE_DONE);
    write_journal(&journal)
}

fn read_journal(state_path: &Path) -> Result<Option<MigrationJournal>> {
    let path = journal_path(state_path);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let journal = serde_json::from_str(&text)
        .map_err(|error| Error::InvalidInput(format!("cannot parse migration journal: {error}")))?;
    Ok(Some(journal))
}

fn write_journal(journal: &MigrationJournal) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|error| {
        Error::InvalidInput(format!("cannot serialize migration journal: {error}"))
    })?;
    write_file_atomic(&journal_path(&journal.state_path), &bytes, 0o600)
}

fn migration_event(event: &str, decision: &str, reason_code: &str, ts: i64) -> AuditEvent {
    AuditEvent {
        ts,
        event: event.to_string(),
        approval_id: None,
        token_id: None,
        agent: None,
        source_id: None,
        tool: "legacy_migration".to_string(),
        profile_id: None,
        profile_version: None,
        route_id: None,
        route_version: None,
        request_digest: None,
        channel: None,
        decision: decision.to_string(),
        actor: "admin-cli".to_string(),
        high_risk: false,
        reason_code: Some(reason_code.to_string()),
        reason: None,
        duration_ms: None,
        row_count: None,
        error_code: None,
    }
}

fn audit_active(state_path: &Path, event: &str, decision: &str, reason_code: &str) -> Result<()> {
    if !state_path.exists() {
        return Ok(());
    }
    let store = Store::open_admin(state_path)?;
    store.record_audit_event(&migration_event(event, decision, reason_code, now_ms()))
}

fn create_snapshot_dir(state_dir: &Path) -> Result<PathBuf> {
    let root = state_dir.join("legacy-migration");
    fs::create_dir_all(&root).map_err(io_error)?;
    set_mode(&root, 0o700)?;
    let stamp = now_ms();
    for attempt in 0..1000 {
        let candidate = if attempt == 0 {
            root.join(stamp.to_string())
        } else {
            root.join(format!("{stamp}-{attempt}"))
        };
        if !candidate.exists() {
            fs::create_dir(&candidate).map_err(io_error)?;
            set_mode(&candidate, 0o700)?;
            return Ok(candidate);
        }
    }
    Err(Error::Conflict(
        "cannot allocate a migration snapshot directory".to_string(),
    ))
}

fn write_sanitized_config(snapshot_config: &Path, config_path: &Path) -> Result<()> {
    let text = fs::read_to_string(snapshot_config).map_err(io_error)?;
    let parsed: toml::Value = toml::from_str(&text).map_err(|error| {
        Error::InvalidInput(format!("cannot parse snapshot legacy config: {error}"))
    })?;
    let server = parsed
        .get("server")
        .cloned()
        .ok_or_else(|| Error::InvalidInput("legacy config is missing [server]".to_string()))?;
    if !server.is_table() {
        return Err(Error::InvalidInput(
            "legacy [server] is not a table".to_string(),
        ));
    }
    let mut root = toml::map::Map::new();
    root.insert("server".to_string(), server);
    let rendered = toml::to_string(&toml::Value::Table(root))
        .map_err(|error| Error::InvalidInput(format!("cannot render sanitized config: {error}")))?;
    write_file_atomic(config_path, rendered.as_bytes(), 0o600)
}

fn copy_sqlite_set(source: &Path, target: &Path) -> Result<()> {
    copy_file_secure(source, target)?;
    for suffix in ["-wal", "-shm"] {
        let source_sidecar = sidecar(source, suffix);
        if source_sidecar.exists() {
            copy_file_secure(&source_sidecar, &sidecar(target, suffix))?;
        }
    }
    Ok(())
}

fn remove_sqlite_set(path: &Path) -> Result<()> {
    remove_file_if_exists(path)?;
    remove_sqlite_sidecars(path)
}

fn remove_sqlite_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        remove_file_if_exists(&sidecar(path, suffix))?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn copy_file_secure(source: &Path, target: &Path) -> Result<()> {
    fs::copy(source, target).map_err(io_error)?;
    set_mode(target, 0o600)
}

fn write_file_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::InvalidInput("target path has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::InvalidInput("target path has no file name".to_string()))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.tmp"));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(io_error)?;
        set_mode(&temporary, mode)?;
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
    }
    replace_file(&temporary, path)
}

fn replace_file(source: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(target).map_err(io_error)?;
    }
    fs::rename(source, target).map_err(io_error)?;
    if let Some(parent) = target.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .map_err(io_error)?
        .sync_all()
        .map_err(io_error)
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_error)?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn injected(options: &MigrateOptions, point: FailPoint) -> Result<()> {
    if options.fail_after == Some(point) {
        Err(Error::InvalidInput(INJECTED_FAILURE.to_string()))
    } else {
        Ok(())
    }
}

fn is_injected(error: &Error) -> bool {
    matches!(error, Error::InvalidInput(message) if message == INJECTED_FAILURE)
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn io_error(error: std::io::Error) -> Error {
    Error::InvalidInput(format!("migration file operation failed: {error}"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
