use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use mcp_db::{
    guard_startup, journal_path, recover_legacy_migration, run_migration, ApprovalPolicy,
    CredentialResolver, EncryptedFileResolver, Environment, Error, FailPoint, MigrateOptions,
    ProfilePurpose, ProvisionLimits, RecoveryOutcome, SourceStatus, Store, Tool,
};
use rusqlite::Connection;
use tempfile::TempDir;

static ENV_COUNTER: AtomicUsize = AtomicUsize::new(0);

const LEGACY_SCHEMA: &str = r#"
CREATE TABLE approvals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  agent TEXT NOT NULL,
  source TEXT NOT NULL,
  tool TEXT NOT NULL,
  sql TEXT NOT NULL,
  params_json TEXT,
  status TEXT NOT NULL,
  channel TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  decided_at INTEGER
);
CREATE INDEX approvals_status ON approvals(status, created_at);
CREATE TABLE audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  agent TEXT NOT NULL,
  source TEXT,
  tool TEXT NOT NULL,
  sql TEXT,
  params_json TEXT,
  duration_ms INTEGER,
  row_count INTEGER,
  decision TEXT NOT NULL,
  channel TEXT,
  reason TEXT
);
"#;

struct Fixture {
    _dir: TempDir,
    config: PathBuf,
    target: PathBuf,
    credential_store: PathBuf,
    master_key: PathBuf,
    legacy_state: PathBuf,
    tokens: BTreeMap<String, String>,
}

fn unique_env(label: &str) -> String {
    let counter = ENV_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(
        "MCPDB_T03_{}_{}_{}",
        std::process::id(),
        counter,
        label.to_ascii_uppercase()
    )
}

fn fixture(driver: &str, set_tokens: bool) -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let config = dir.path().join("legacy.toml");
    let target = dir.path().join("target").join("state.db");
    let credential_store = dir.path().join("credentials.enc");
    let master_key = dir.path().join("master.key");
    let legacy_state = dir.path().join("legacy-state.db");

    let reader_env = unique_env("reader");
    let writer_env = unique_env("writer");
    let approver_env = unique_env("approver");
    let tokens = BTreeMap::from([
        ("reader".to_string(), format!("mcpdb_test_{reader_env}")),
        ("writer".to_string(), format!("mcpdb_test_{writer_env}")),
        ("approver".to_string(), format!("mcpdb_test_{approver_env}")),
    ]);
    if set_tokens {
        for (agent, token) in &tokens {
            let name = match agent.as_str() {
                "reader" => &reader_env,
                "writer" => &writer_env,
                _ => &approver_env,
            };
            std::env::set_var(name, token);
        }
    }

    let toml = format!(
        r#"[server]
host = "127.0.0.1"
port = 8787
admin_token_env = "MCPDB_ADMIN_TOKEN"
state_path = "./legacy-state.db"
approval_timeout_ms = 120000

[[sources]]
id = "main"
driver = "{driver}"
dsn_readonly = "postgres://ro_main:secret@127.0.0.1:5432/main"
dsn_readwrite = "postgres://rw_main:secret@127.0.0.1:5432/main"
max_rows = 500
statement_timeout_ms = 15000

[[sources]]
id = "analytics"
driver = "{driver}"
dsn_readonly = "postgres://ro_analytics:secret@127.0.0.1:5432/analytics"
dsn_readwrite = "postgres://rw_analytics:secret@127.0.0.1:5432/analytics"
max_rows = 1000

[agents.reader]
token_env = "{reader_env}"
sources = {{ main = "readonly" }}

[agents.writer]
token_env = "{writer_env}"
sources = {{ main = "readwrite" }}

[agents.approver]
token_env = "{approver_env}"
sources = {{ main = "approval", analytics = "readonly" }}
"#
    );
    fs::write(&config, toml).expect("write legacy config");
    let key = [3_u8; 32];
    let key_hex = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    fs::write(&master_key, key_hex).expect("write master key");

    let connection = Connection::open(&legacy_state).expect("open legacy state");
    connection
        .execute_batch(LEGACY_SCHEMA)
        .expect("legacy schema");
    connection
        .execute(
            "INSERT INTO approvals
               (agent, source, tool, sql, params_json, status, channel, created_at, expires_at, decided_at)
             VALUES ('writer', 'main', 'mutate_sql', 'update t set a = 1', NULL, 'denied', 'cli', 1, 2, 3)",
            [],
        )
        .expect("legacy approval");
    connection
        .execute(
            "INSERT INTO audit
               (ts, agent, source, tool, sql, params_json, duration_ms, row_count, decision, channel, reason)
             VALUES ('2026-01-01T00:00:00Z', 'reader', 'main', 'query_sql', 'select 1', NULL, 4, 1, 'allowed', 'mcp', NULL)",
            [],
        )
        .expect("legacy audit");
    drop(connection);

    Fixture {
        _dir: dir,
        config,
        target,
        credential_store,
        master_key,
        legacy_state,
        tokens,
    }
}

fn options(fixture: &Fixture) -> MigrateOptions {
    MigrateOptions {
        legacy_config: fixture.config.clone(),
        environment: Environment::NonProduction,
        state_path: fixture.target.clone(),
        credential_store: fixture.credential_store.clone(),
        master_key_file: fixture.master_key.clone(),
        fail_after: None,
    }
}

fn journal(fixture: &Fixture) -> serde_json::Value {
    let text = fs::read_to_string(journal_path(&fixture.target)).expect("read journal");
    serde_json::from_str(&text).expect("parse journal")
}

fn grant_map(path: &Path, agent: &str) -> BTreeMap<String, ApprovalPolicy> {
    let connection = Connection::open(path).expect("open state for grants");
    let mut statement = connection
        .prepare(
            "SELECT g.tool, g.policy
               FROM tool_grants g JOIN tokens t ON t.id = g.token_id
              WHERE t.agent = ?",
        )
        .expect("prepare grants");
    statement
        .query_map([agent], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query grants")
        .map(|row| {
            let (tool, policy) = row.expect("grant row");
            (tool, ApprovalPolicy::parse(&policy).expect("grant policy"))
        })
        .collect()
}

#[cfg(unix)]
fn assert_mode_600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{} must be 0600", path.display());
}

#[test]
fn migration_imports_sources_and_fixed_grant_matrix() {
    let fixture = fixture("postgres", true);
    let report = run_migration(&options(&fixture)).expect("migration succeeds");
    assert_eq!(report.sources, vec!["main", "analytics"]);
    assert_eq!(report.agents, vec!["approver", "reader", "writer"]);
    assert_eq!(report.warnings.len(), 2);

    let store = Store::open_admin(&fixture.target).expect("open migrated state");
    let sources = store.list_all_sources().expect("sources");
    assert_eq!(sources.len(), 2);
    for source in &sources {
        assert_eq!(source.status, SourceStatus::Archived);
        assert_eq!(source.environment, Environment::NonProduction);
    }

    let profiles = store.list_profiles("main").expect("profiles");
    assert_eq!(profiles.len(), 3);
    let query = profiles
        .iter()
        .find(|profile| profile.purpose == ProfilePurpose::Query)
        .expect("query profile");
    let mutate = profiles
        .iter()
        .find(|profile| profile.purpose == ProfilePurpose::Mutate)
        .expect("mutate profile");
    let schema = profiles
        .iter()
        .find(|profile| profile.purpose == ProfilePurpose::Schema)
        .expect("schema profile");
    assert_eq!(query.credential_ref, "keyring:legacy/main/query");
    assert_eq!(mutate.credential_ref, "keyring:legacy/main/mutate");
    assert_eq!(schema.credential_ref, "keyring:legacy/main/schema");
    assert_eq!(query.statement_timeout_ms, Some(15_000));
    assert_eq!(mutate.statement_timeout_ms, Some(15_000));
    assert_eq!(schema.statement_timeout_ms, Some(15_000));
    let analytics = store.list_profiles("analytics").expect("profiles");
    assert_eq!(analytics.len(), 3);
    for profile in analytics {
        assert_eq!(profile.statement_timeout_ms, None);
    }
    assert!(profiles
        .iter()
        .all(|profile| profile.purpose != ProfilePurpose::Provision));

    for tool in [
        Tool::ListTables,
        Tool::DescribeTable,
        Tool::ExplainQuery,
        Tool::QuerySql,
        Tool::MutateSql,
        Tool::ExecuteDdl,
    ] {
        let route = store.get_route("main", tool).expect("route");
        assert_eq!(route.min_approval_policy, ApprovalPolicy::Direct);
    }
    assert!(store.get_route("main", Tool::CreateDatabase).is_err());

    let reader = grant_map(&fixture.target, "reader");
    assert_eq!(reader.len(), 4);
    assert!(reader
        .values()
        .all(|policy| *policy == ApprovalPolicy::Direct));

    let writer = grant_map(&fixture.target, "writer");
    assert_eq!(writer.len(), 6);
    assert_eq!(writer["mutate_sql"], ApprovalPolicy::Direct);
    assert_eq!(writer["execute_ddl"], ApprovalPolicy::Direct);

    let approver = grant_map(&fixture.target, "approver");
    assert_eq!(approver.len(), 6);
    assert_eq!(approver["mutate_sql"], ApprovalPolicy::Approval);
    assert_eq!(approver["execute_ddl"], ApprovalPolicy::Approval);
    assert_eq!(approver["query_sql"], ApprovalPolicy::Direct);

    for agent in ["reader", "writer", "approver"] {
        let grants = grant_map(&fixture.target, agent);
        assert!(!grants.contains_key("create_database"));
        let token = &fixture.tokens[agent];
        let identity = store.authenticate(token).expect("token authenticates");
        assert_eq!(identity.agent, agent);
    }

    let resolver = EncryptedFileResolver::open(&fixture.credential_store, &fixture.master_key)
        .expect("open credentials");
    assert_eq!(
        resolver.resolve("keyring:legacy/main/query").unwrap(),
        "postgres://ro_main:secret@127.0.0.1:5432/main"
    );
    assert_eq!(
        resolver.resolve("keyring:legacy/main/mutate").unwrap(),
        "postgres://rw_main:secret@127.0.0.1:5432/main"
    );
    assert_eq!(
        resolver.resolve("keyring:legacy/main/schema").unwrap(),
        "postgres://rw_main:secret@127.0.0.1:5432/main"
    );
    assert_eq!(
        resolver.resolve("keyring:legacy/analytics/query").unwrap(),
        "postgres://ro_analytics:secret@127.0.0.1:5432/analytics"
    );

    let active = fs::read_to_string(&fixture.config).expect("active config");
    assert!(active.contains("[server]"));
    for forbidden in [
        "dsn_readonly",
        "dsn_readwrite",
        "postgres://",
        "[[sources]]",
        "[agents.",
    ] {
        assert!(
            !active.contains(forbidden),
            "active config contains {forbidden}"
        );
    }

    let journal = journal(&fixture);
    assert_eq!(journal["stage"], "done");
    let snapshot_config =
        PathBuf::from(journal["snapshot_config"].as_str().expect("snapshot path"));
    let backup = fs::read_to_string(&snapshot_config).expect("backup config");
    assert!(backup.contains("dsn_readonly"));
    assert!(backup.contains("[[sources]]"));
    #[cfg(unix)]
    assert_mode_600(&snapshot_config);

    let connection = Connection::open(&fixture.target).expect("open migrated db");
    let approvals: i64 = connection
        .query_row("SELECT COUNT(*) FROM approvals", [], |row| row.get(0))
        .expect("legacy approvals");
    let audits: i64 = connection
        .query_row("SELECT COUNT(*) FROM audit", [], |row| row.get(0))
        .expect("legacy audits");
    assert_eq!(approvals, 1);
    assert_eq!(audits, 1);

    let events = store.list_recent_audit(50).expect("audit events");
    let names = events
        .iter()
        .map(|event| event.event.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"legacy_migration_started"));
    assert!(names.contains(&"legacy_migration_completed"));
    for event in &events {
        if event.event.starts_with("legacy_migration") {
            assert_eq!(event.actor, "admin-cli");
            assert!(!event.event.contains("secret"));
        }
    }
    assert!(!format!("{events:?}").contains("secret"));
}

#[test]
fn migration_can_replace_the_legacy_state_database_in_place() {
    let mut fixture = fixture("postgres", true);
    let legacy_state = fixture.legacy_state.clone();
    fixture.target = legacy_state;
    run_migration(&options(&fixture)).expect("in-place migration");
    let store = Store::open_admin(&fixture.target).expect("open migrated state");
    assert!(store
        .list_all_sources()
        .expect("sources")
        .iter()
        .all(|source| source.status == SourceStatus::Archived));
    let connection = Connection::open(&fixture.target).expect("open state");
    let approvals: i64 = connection
        .query_row("SELECT COUNT(*) FROM approvals", [], |row| row.get(0))
        .expect("legacy approvals");
    assert_eq!(approvals, 1);
    assert_eq!(journal(&fixture)["stage"], "done");
}

#[test]
fn production_environment_with_write_modes_fails_before_targets() {
    let fixture = fixture("postgres", true);
    let original = fs::read_to_string(&fixture.config).expect("config");
    let mut options = options(&fixture);
    options.environment = Environment::Production;
    let error = run_migration(&options).expect_err("production writes are refused");
    let message = error.to_string();
    assert!(message.contains("production"));
    assert!(message.contains("nonproduction"));
    assert_eq!(
        fs::read_to_string(&fixture.config).expect("config"),
        original
    );
    assert!(!fixture.target.exists());
    assert!(!journal_path(&fixture.target).exists());
}

#[test]
fn nonempty_wal_blocks_migration_before_targets() {
    for stale in ["legacy", "target"] {
        let fixture = fixture("postgres", true);
        let original = fs::read_to_string(&fixture.config).expect("config");
        let path = if stale == "legacy" {
            fixture.legacy_state.clone()
        } else {
            fixture.target.clone()
        };
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(wal_path(&path), b"uncheckpointed").expect("write stale wal");
        let error = run_migration(&options(&fixture)).expect_err("stale WAL is refused");
        assert!(
            error.to_string().contains("WAL"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(&fixture.config).expect("config"),
            original
        );
        assert!(!fixture.target.exists());
        assert!(!journal_path(&fixture.target).exists());
    }
}

#[test]
fn non_postgres_driver_fails_before_touching_targets() {
    let fixture = fixture("mysql", true);
    let original = fs::read_to_string(&fixture.config).expect("config");
    let error = run_migration(&options(&fixture)).expect_err("mysql is unsupported");
    assert!(error.to_string().contains("unsupported driver"));
    assert_eq!(
        fs::read_to_string(&fixture.config).expect("config"),
        original
    );
    assert!(!fixture.target.exists());
    assert!(!journal_path(&fixture.target).exists());
    assert!(fixture.legacy_state.exists());
}

#[test]
fn missing_token_environment_fails_without_cutover() {
    let fixture = fixture("postgres", false);
    let original = fs::read_to_string(&fixture.config).expect("config");
    let error = run_migration(&options(&fixture)).expect_err("token env missing");
    assert!(error.to_string().contains("is not set"));
    assert_eq!(
        fs::read_to_string(&fixture.config).expect("config"),
        original
    );
    assert!(!fixture.target.exists());
    assert!(!journal_path(&fixture.target).exists());
}

#[test]
fn repeated_migration_is_refused_without_overwrite() {
    let fixture = fixture("postgres", true);
    run_migration(&options(&fixture)).expect("first migration");
    let sanitized = fs::read_to_string(&fixture.config).expect("config");
    let error = run_migration(&options(&fixture)).expect_err("second run refused");
    assert!(matches!(error, Error::Conflict(_)));
    assert_eq!(
        fs::read_to_string(&fixture.config).expect("config"),
        sanitized
    );
    let store = Store::open_admin(&fixture.target).expect("state");
    assert_eq!(store.list_all_sources().expect("sources").len(), 2);
    for source in ["main", "analytics"] {
        assert_eq!(store.list_profiles(source).expect("profiles").len(), 3);
    }
}

#[test]
fn crash_before_commit_rolls_back_and_blocks_startup() {
    let fixture = fixture("postgres", true);
    let original = fs::read_to_string(&fixture.config).expect("config");
    let mut options = options(&fixture);
    options.fail_after = Some(FailPoint::BeforeCommit);
    run_migration(&options).expect_err("injected crash");
    assert_eq!(journal(&fixture)["stage"], "staging_ready");
    assert!(guard_startup(&fixture.target).is_err());
    assert_eq!(
        fs::read_to_string(&fixture.config).expect("config"),
        original
    );

    let outcome = recover_legacy_migration(&fixture.target).expect("recovery");
    assert_eq!(outcome, RecoveryOutcome::RolledBack);
    assert_eq!(journal(&fixture)["stage"], "failed_precommit");
    let staging = PathBuf::from(
        journal(&fixture)["staging_state"]
            .as_str()
            .expect("staging"),
    );
    assert!(!staging.exists());
    assert!(!fixture.target.exists());
    assert_eq!(
        fs::read_to_string(&fixture.config).expect("config"),
        original
    );
    assert!(guard_startup(&fixture.target).is_ok());
    assert_eq!(
        recover_legacy_migration(&fixture.target).expect("second recovery"),
        RecoveryOutcome::NothingToDo
    );
}

#[test]
fn crash_after_state_commit_completes_toml_on_recovery() {
    let fixture = fixture("postgres", true);
    let mut options = options(&fixture);
    options.fail_after = Some(FailPoint::AfterStateCommit);
    run_migration(&options).expect_err("injected crash");
    assert_eq!(journal(&fixture)["stage"], "state_committed");
    assert!(fixture.target.exists());
    assert!(guard_startup(&fixture.target).is_err());
    let active = fs::read_to_string(&fixture.config).expect("config");
    assert!(
        active.contains("dsn_readonly"),
        "TOML replacement did not run"
    );

    let outcome = recover_legacy_migration(&fixture.target).expect("recovery");
    assert_eq!(outcome, RecoveryOutcome::Completed);
    assert_eq!(journal(&fixture)["stage"], "done");
    let active = fs::read_to_string(&fixture.config).expect("config");
    assert!(active.contains("[server]"));
    assert!(!active.contains("dsn_readonly"));
    assert!(!active.contains("[[sources]]"));
    assert!(guard_startup(&fixture.target).is_ok());

    let store = Store::open_admin(&fixture.target).expect("state");
    assert!(store
        .list_all_sources()
        .expect("sources")
        .iter()
        .all(|source| source.status == SourceStatus::Archived));
}

#[test]
fn only_archived_sources_can_change_environment() {
    let fixture = fixture("postgres", true);
    run_migration(&options(&fixture)).expect("migration");
    let store = Store::open_admin(&fixture.target).expect("state");
    let source = store.get_source("main").expect("source");
    assert_eq!(source.status, SourceStatus::Archived);
    assert_eq!(source.environment, Environment::NonProduction);

    let error = store
        .set_source_environment("main", Environment::Production)
        .expect_err("write grants block production classification");
    assert!(matches!(error, Error::InvalidInput(_)));
    assert_eq!(
        store.get_source("main").expect("source").environment,
        Environment::NonProduction
    );

    let analytics = store
        .set_source_environment("analytics", Environment::Production)
        .expect("readonly-only source can become production");
    assert_eq!(analytics.environment, Environment::Production);

    store
        .restore_source("main", &ProvisionLimits::default(), "admin-cli", now_ms())
        .expect("restore after classification");
    let restored = store.get_source("main").expect("source");
    assert_eq!(restored.environment, Environment::NonProduction);

    let error = store
        .set_source_environment("main", Environment::Production)
        .expect_err("active source is immutable");
    assert!(matches!(error, Error::Conflict(_)));
    assert_eq!(
        store.get_source("main").expect("source").environment,
        Environment::NonProduction
    );
}

fn wal_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push("-wal");
    PathBuf::from(value)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
