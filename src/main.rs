use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use mcp_db::{
    check_health, ApprovalPolicy, AuditEvent, Config, CredentialSink, Driver, DriverExecutors,
    EncryptedFileResolver, Environment, MetadataProvider, NewGrant, NewProfile, NewRoute,
    NewSource, PostgresExecutor, PostgresMetadata, ProfilePurpose, SqlExecutor, SqliteExecutor,
    SqliteMetadata, Store, Tool, UnavailableExecutor, UnavailableMetadata, ADMIN_LOGIN_TTL_MS,
    DEFAULT_ADDR, HEALTHCHECK_ADDR,
};

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mcp-db: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next().as_deref() {
        Some("--healthcheck") => {
            let addr =
                env::var("MCPDB_HEALTHCHECK_ADDR").unwrap_or_else(|_| HEALTHCHECK_ADDR.to_string());
            check_health(&addr).map_err(|err| format!("healthcheck failed: {err}"))
        }
        Some("--version") => {
            println!("mcp-db {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help") => {
            print_help();
            Ok(())
        }
        Some("migrate-legacy") => run_migrate(args),
        Some("recover-legacy-migration") => run_recover(args),
        Some(command)
            if matches!(
                command,
                "admin-login-token"
                    | "admin-key"
                    | "sources"
                    | "profiles"
                    | "routes"
                    | "tokens"
                    | "grants"
                    | "audit"
                    | "approvals"
                    | "approve"
                    | "deny"
                    | "archive-source"
                    | "restore-source"
                    | "reservations"
                    | "settle-reservation"
            ) =>
        {
            run_admin(command, args)
        }
        Some(other) => Err(format!("unknown argument: {other}")),
        None => {
            let config = Config::from_env().map_err(|error| error.to_string())?;
            mcp_db::guard_startup(&config.state_path)
                .map_err(|error| format!("state init failed: {error}"))?;
            let store = Store::open(&config.state_path)
                .map_err(|error| format!("state init failed: {error}"))?;
            let store = Arc::new(Mutex::new(store));
            let metadata: Arc<dyn MetadataProvider>;
            let executor: Arc<dyn SqlExecutor>;
            let provisioner: Arc<dyn mcp_db::provision::Provisioner>;
            let credentials: Option<Arc<dyn mcp_db::credentials::CredentialSink>>;
            let admin_resolver: Option<Arc<dyn mcp_db::credentials::CredentialResolver>>;
            match (&config.credential_store, &config.master_key_file) {
                (Some(store_path), Some(key_path)) => {
                    let resolver = open_credential_resolver(store_path, key_path)?;
                    let resolver = Arc::new(resolver);
                    credentials = Some(resolver.clone());
                    admin_resolver = Some(resolver.clone());
                    let connections = Arc::new(mcp_db::postgres::ProfileConnections::default());
                    let postgres_metadata: Arc<dyn MetadataProvider> = Arc::new(
                        PostgresMetadata::with_connections(resolver.clone(), connections.clone()),
                    );
                    let postgres_executor: Arc<dyn SqlExecutor> = Arc::new(
                        PostgresExecutor::with_connections(resolver.clone(), connections.clone()),
                    );
                    let sqlite_metadata: Arc<dyn MetadataProvider> = Arc::new(
                        SqliteMetadata::with_connections(resolver.clone(), connections.clone()),
                    );
                    let sqlite_executor: Arc<dyn SqlExecutor> = Arc::new(
                        SqliteExecutor::with_connections(resolver.clone(), connections),
                    );
                    let routers = Arc::new(DriverExecutors::new(
                        store.clone(),
                        postgres_executor,
                        postgres_metadata,
                        sqlite_executor,
                        sqlite_metadata,
                    ));
                    metadata = routers.clone();
                    executor = routers;
                    provisioner = Arc::new(mcp_db::provision::PostgresProvisioner::new(
                        resolver.clone(),
                        resolver,
                    ));
                }
                _ => {
                    metadata = Arc::new(UnavailableMetadata);
                    executor = Arc::new(UnavailableExecutor);
                    provisioner = Arc::new(mcp_db::provision::UnavailableProvisioner(
                        "credential store is not configured".to_string(),
                    ));
                    credentials = None;
                    admin_resolver = None;
                }
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("runtime init failed: {error}"))?;
            runtime
                .block_on(mcp_db::transport::http::run(
                    config,
                    store,
                    metadata,
                    executor,
                    provisioner,
                    credentials,
                    admin_resolver,
                ))
                .map_err(|error| format!("server error: {error}"))
        }
    }
}

fn run_migrate(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut config_path: Option<String> = None;
    let mut environment = Environment::Production;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--config" => {
                config_path = Some(args.next().ok_or("--config requires a legacy TOML path")?);
            }
            "--environment" => {
                environment = Environment::parse(
                    &args
                        .next()
                        .ok_or("--environment requires production|nonproduction")?,
                )
                .map_err(|error| error.to_string())?;
            }
            other => return Err(format!("unknown migrate-legacy argument: {other}")),
        }
    }
    let config_path = config_path.ok_or("migrate-legacy requires --config <legacy.toml>")?;
    let config = Config::from_env().map_err(|error| error.to_string())?;
    let credential_store = config.credential_store.ok_or(
        "MCPDB_CREDENTIAL_STORE and MCPDB_MASTER_KEY_FILE must be configured for migration",
    )?;
    let master_key_file = config.master_key_file.ok_or(
        "MCPDB_CREDENTIAL_STORE and MCPDB_MASTER_KEY_FILE must be configured for migration",
    )?;
    let report = mcp_db::run_migration(&mcp_db::MigrateOptions {
        legacy_config: PathBuf::from(config_path),
        environment,
        state_path: config.state_path,
        credential_store,
        master_key_file,
        fail_after: None,
    })
    .map_err(|error| error.to_string())?;
    println!(
        "migrated {} sources and {} agents; sources are archived",
        report.sources.len(),
        report.agents.len()
    );
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    println!("snapshot: {}", report.snapshot_dir.display());
    Ok(())
}

fn run_recover(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    if let Some(extra) = args.next() {
        return Err(format!(
            "recover-legacy-migration takes no arguments (got {extra})"
        ));
    }
    let config = Config::from_env().map_err(|error| error.to_string())?;
    let outcome =
        mcp_db::recover_legacy_migration(&config.state_path).map_err(|error| error.to_string())?;
    let label = match outcome {
        mcp_db::RecoveryOutcome::RolledBack => "rolled back before the commit point",
        mcp_db::RecoveryOutcome::Completed => "completed the committed cutover",
        mcp_db::RecoveryOutcome::NothingToDo => "nothing to do",
    };
    println!("migration recovery: {label}");
    Ok(())
}

/// Opens the service credential store, creating an empty one on first use. The
/// master key file must already exist and parse; anything else fails startup.
fn open_credential_resolver(
    store_path: &Path,
    key_path: &Path,
) -> Result<EncryptedFileResolver, String> {
    EncryptedFileResolver::open_or_create(store_path, key_path)
        .map_err(|error| format!("credential init failed: {error}"))
}

fn print_help() {
    println!(
"mcp-db {}\n\nUsage:\n  mcp-db                 Start the MCP service (MCPDB_ADDR, default {DEFAULT_ADDR})\n                         and the local admin UI (MCPDB_ADMIN_ADDR, default {})\n  mcp-db --healthcheck   Check a running service (MCPDB_HEALTHCHECK_ADDR)\n  mcp-db --version\n  mcp-db --help\n\nLegacy v1 migration (stop the old service first):\n  mcp-db migrate-legacy --config <legacy.toml> [--environment production|nonproduction]\n  mcp-db recover-legacy-migration\n\nLocal admin commands (use the same MCPDB_STATE_PATH as the service):\n  mcp-db admin-login-token        Print a one-time web login token (valid 5 minutes)\n  mcp-db admin-key create <label> Create a permanent admin key (shown once, revocable)\n  mcp-db admin-key list           List admin keys (never shows the secret)\n  mcp-db admin-key revoke <id>    Revoke an admin key immediately\n  mcp-db sources                  List all sources\n  mcp-db sources add <id> <production|nonproduction> [postgres|sqlite]\n  mcp-db sources set-environment <id> <production|nonproduction>  (archived only)\n  mcp-db sources delete <id>          Delete a source's control-plane records (physical database untouched)\n  mcp-db profiles <source>        List profiles of one source\n  mcp-db profiles add <source> <name> <query|mutate|schema|provision> <credential_ref> [max_connections] [statement_timeout_ms]\n  mcp-db routes add <source> <tool> <profile_id> [direct|approval]\n  mcp-db tokens issue <agent>     Issue an agent token (plaintext shown once)\n  mcp-db tokens revoke <token_id>\n  mcp-db grants <token_id>        List grants of one token\n  mcp-db grants add <token_id> <source> <tool> [direct|approval]\n  mcp-db grants revoke <token_id> <source> <tool>\n  mcp-db audit [limit]            List recent audit events (default 50)\n  mcp-db approvals [limit]        List pending approvals\n  mcp-db approve <id>             Approve one pending request\n  mcp-db deny <id>                Deny one pending request\n  mcp-db archive-source <id>      Archive a source without dropping the database\n  mcp-db restore-source <id>      Restore an archived source after quota checks\n  mcp-db reservations             List recent provisioning reservations\n  mcp-db settle-reservation <id> release|commit",
        env!("CARGO_PKG_VERSION"),
        mcp_db::ADMIN_ADDR
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_id(command: &str, value: Option<String>) -> Result<i64, String> {
    value
        .ok_or_else(|| format!("{command} requires an id"))?
        .parse()
        .map_err(|_| format!("{command} id must be a number"))
}

fn run_admin(command: &str, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let config = Config::from_env().map_err(|error| error.to_string())?;
    let store = Store::open_admin(&config.state_path)
        .map_err(|error| format!("state init failed: {error}"))?;
    let now = now_ms();
    match command {
        "admin-login-token" => {
            let token = store
                .create_admin_login_token(now, ADMIN_LOGIN_TTL_MS)
                .map_err(|error| error.to_string())?;
            // Plaintext is printed exactly once; only the hash is persisted.
            println!("{token}");
            eprintln!("one-time admin login token, valid for 5 minutes");
            Ok(())
        }
        "admin-key" => match args.next().as_deref() {
            Some("create") => {
                let label = args
                    .next()
                    .ok_or("admin-key create requires a label, e.g. laptop")?;
                let key = store
                    .create_admin_key(&label, now)
                    .map_err(|error| error.to_string())?;
                record_admin_audit(&store, "admin_key_created", "admin_key", Some(&label), None)?;
                // Plaintext is printed exactly once; only the hash is persisted.
                println!("{key}");
                eprintln!(
                    "permanent admin key created; it never expires. Revoke with: mcp-db admin-key revoke <id>"
                );
                Ok(())
            }
            Some("list") | None => {
                let keys = store.list_admin_keys().map_err(|error| error.to_string())?;
                if keys.is_empty() {
                    println!("no admin keys");
                    return Ok(());
                }
                for key in keys {
                    println!(
                        "{}\t{}\tcreated_at={}\tlast_used_at={}\t{}",
                        key.id,
                        key.label,
                        key.created_at,
                        key.last_used_at
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                        if key.revoked_at.is_some() {
                            "revoked"
                        } else {
                            "active"
                        }
                    );
                }
                Ok(())
            }
            Some("revoke") => {
                let id = parse_id("admin-key revoke", args.next())?;
                store
                    .revoke_admin_key(id, now)
                    .map_err(|error| error.to_string())?;
                let id_text = id.to_string();
                record_admin_audit(
                    &store,
                    "admin_key_revoked",
                    "admin_key",
                    Some(&id_text),
                    None,
                )?;
                println!("revoked admin key {id}");
                Ok(())
            }
            Some(other) => Err(format!(
                "unknown admin-key subcommand: {other} (use create, list or revoke)"
            )),
        },
        "sources" => match args.next().as_deref() {
            Some("add") => {
                let id = args.next().ok_or("sources add requires a source id")?;
                let environment = Environment::parse(
                    &args
                        .next()
                        .ok_or("sources add requires production|nonproduction")?,
                )
                .map_err(|error| error.to_string())?;
                let driver = match args.next() {
                    None => Driver::Postgres,
                    Some(value) => Driver::parse(&value).map_err(|error| error.to_string())?,
                };
                let source = store
                    .create_source(&NewSource {
                        id,
                        driver,
                        environment,
                        parent_source_id: None,
                        created_token_id: None,
                        created_agent: None,
                        created_origin: Some("admin-cli".to_string()),
                    })
                    .map_err(|error| error.to_string())?;
                println!(
                    "created source {} ({}, {})",
                    source.id,
                    source.driver.as_str(),
                    source.environment.as_str()
                );
                record_admin_audit(
                    &store,
                    "admin_source_added",
                    "source",
                    Some(source.id.as_str()),
                    None,
                )?;
                Ok(())
            }
            None => {
                let sources = store
                    .list_all_sources()
                    .map_err(|error| error.to_string())?;
                if sources.is_empty() {
                    println!("no sources");
                }
                for source in sources {
                    println!(
                        "{}  {}  {}",
                        source.id,
                        source.environment.as_str(),
                        source.status.as_str()
                    );
                }
                Ok(())
            }
            Some("set-environment") => {
                let id = args
                    .next()
                    .ok_or("sources set-environment requires a source id")?;
                let environment = Environment::parse(
                    &args
                        .next()
                        .ok_or("sources set-environment requires production|nonproduction")?,
                )
                .map_err(|error| error.to_string())?;
                let source = store
                    .set_source_environment(&id, environment)
                    .map_err(|error| error.to_string())?;
                println!(
                    "source {} environment set to {}",
                    source.id,
                    source.environment.as_str()
                );
                record_admin_audit(
                    &store,
                    "admin_source_environment_set",
                    "source",
                    Some(source.id.as_str()),
                    None,
                )?;
                Ok(())
            }
            Some("delete") => {
                let id = args.next().ok_or("sources delete requires a source id")?;
                // Snapshot the credential references before the profiles are gone.
                let references = store
                    .list_profiles(&id)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|profile| profile.credential_ref)
                    .collect::<Vec<_>>();
                let source = store
                    .delete_source(&id, "admin-cli", now)
                    .map_err(|error| error.to_string())?;
                let mut failures = Vec::new();
                match (&config.credential_store, &config.master_key_file) {
                    // The delete is already committed, so a credential-store failure
                    // must not turn the command into an error.
                    (Some(store_path), Some(key_path)) => {
                        match open_credential_resolver(store_path, key_path) {
                            Ok(resolver) => {
                                for reference in &references {
                                    // The store rows are gone, so any remaining profile pointing
                                    // at this reference belongs to another source; keep it.
                                    match store.count_profiles_with_credential(reference) {
                                        Ok(0) => {
                                            if let Err(error) = resolver.remove(reference) {
                                                failures.push(format!(
                                                    "managed credential {reference} could not be removed: {error}"
                                                ));
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(error) => failures.push(format!(
                                            "could not check references for {reference}: {error}"
                                        )),
                                    }
                                }
                            }
                            Err(error) => failures.push(format!(
                                "credential store could not be opened, managed credentials stay in place: {error}"
                            )),
                        }
                    }
                    _ => {
                        eprintln!(
                            "notice: credential store is not configured; managed credentials stay in the credential store"
                        );
                    }
                }
                for failure in &failures {
                    eprintln!("warning: {failure}");
                }
                println!("deleted {} (physical database untouched)", source.id);
                Ok(())
            }
            Some(other) => Err(format!("unknown sources subcommand: {other}")),
        },
        "profiles" => match args.next().as_deref() {
            Some("add") => {
                let source_id = args.next().ok_or("profiles add requires a source id")?;
                let name = args.next().ok_or("profiles add requires a profile name")?;
                let purpose =
                    ProfilePurpose::parse(&args.next().ok_or("profiles add requires a purpose")?)
                        .map_err(|error| error.to_string())?;
                let credential_ref = args
                    .next()
                    .ok_or("profiles add requires a keyring credential reference")?;
                let max_connections = match args.next() {
                    None => None,
                    Some(value) => Some(
                        value
                            .parse::<u32>()
                            .map_err(|_| "max_connections must be a number".to_string())?,
                    ),
                };
                let statement_timeout_ms = match args.next() {
                    None => None,
                    Some(value) => Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| "statement_timeout_ms must be a number".to_string())?,
                    ),
                };
                let profile = store
                    .create_profile(&NewProfile {
                        source_id,
                        name,
                        purpose,
                        credential_ref,
                        max_connections,
                        statement_timeout_ms,
                    })
                    .map_err(|error| error.to_string())?;
                println!(
                    "profile {}  {}/{}  {}  enabled={}",
                    profile.id,
                    profile.source_id,
                    profile.name,
                    profile.purpose.as_str(),
                    profile.enabled
                );
                record_admin_audit(
                    &store,
                    "admin_profile_added",
                    "profile",
                    Some(profile.source_id.as_str()),
                    None,
                )?;
                Ok(())
            }
            Some(source_id) => {
                let profiles = store
                    .list_profiles(source_id)
                    .map_err(|error| error.to_string())?;
                if profiles.is_empty() {
                    println!("no profiles for source {source_id}");
                }
                for profile in profiles {
                    println!(
                        "{}  {}  {}  enabled={}  version={}",
                        profile.id,
                        profile.name,
                        profile.purpose.as_str(),
                        profile.enabled,
                        profile.version
                    );
                }
                Ok(())
            }
            None => Err("profiles requires a source id or 'add'".to_string()),
        },
        "routes" => match args.next().as_deref() {
            Some("add") => {
                let source_id = args.next().ok_or("routes add requires a source id")?;
                let tool = Tool::parse(&args.next().ok_or("routes add requires a tool name")?)
                    .map_err(|error| error.to_string())?;
                let profile_id = args
                    .next()
                    .ok_or("routes add requires a profile id")?
                    .parse::<i64>()
                    .map_err(|_| "profile id must be a number".to_string())?;
                let policy = match args.next() {
                    None => default_policy(tool),
                    Some(value) => {
                        ApprovalPolicy::parse(&value).map_err(|error| error.to_string())?
                    }
                };
                let route = store
                    .create_route(&NewRoute {
                        source_id,
                        tool,
                        profile_id,
                        min_approval_policy: policy,
                    })
                    .map_err(|error| error.to_string())?;
                println!(
                    "route {}  {}/{}  profile={}  min_policy={}",
                    route.id,
                    route.source_id,
                    route.tool.as_str(),
                    route.profile_id,
                    route.min_approval_policy.as_str()
                );
                record_admin_audit(
                    &store,
                    "admin_route_added",
                    "route",
                    Some(route.source_id.as_str()),
                    None,
                )?;
                Ok(())
            }
            Some(other) => Err(format!("unknown routes subcommand: {other}")),
            None => Err("routes requires the 'add' subcommand".to_string()),
        },
        "tokens" => match args.next().as_deref() {
            Some("issue") => {
                let agent = args.next().ok_or("tokens issue requires an agent name")?;
                let issued = store
                    .create_token(&agent)
                    .map_err(|error| error.to_string())?;
                println!("token_id: {}", issued.token_id);
                println!("token: {}", issued.token);
                eprintln!("store the token now; it cannot be shown again");
                record_admin_audit(
                    &store,
                    "admin_token_issued",
                    "token",
                    None,
                    Some(issued.token_id.as_str()),
                )?;
                Ok(())
            }
            Some("revoke") => {
                let token_id = args.next().ok_or("tokens revoke requires a token id")?;
                store
                    .revoke_token(&token_id)
                    .map_err(|error| error.to_string())?;
                println!("revoked {token_id}");
                record_admin_audit(
                    &store,
                    "admin_token_revoked",
                    "token",
                    None,
                    Some(token_id.as_str()),
                )?;
                Ok(())
            }
            Some(other) => Err(format!("unknown tokens subcommand: {other}")),
            None => Err("tokens requires 'issue' or 'revoke'".to_string()),
        },
        "grants" => match args.next().as_deref() {
            Some("add") => {
                let token_id = args.next().ok_or("grants add requires a token id")?;
                let source_id = args.next().ok_or("grants add requires a source id")?;
                let tool = Tool::parse(&args.next().ok_or("grants add requires a tool name")?)
                    .map_err(|error| error.to_string())?;
                let policy = match args.next() {
                    None => None,
                    Some(value) => {
                        Some(ApprovalPolicy::parse(&value).map_err(|error| error.to_string())?)
                    }
                };
                let grant = store
                    .create_grant(&NewGrant {
                        token_id,
                        source_id,
                        tool,
                        policy,
                    })
                    .map_err(|error| error.to_string())?;
                println!(
                    "grant {}  {}/{}  policy={}",
                    grant.token_id,
                    grant.source_id,
                    grant.tool.as_str(),
                    grant.policy.as_str()
                );
                record_admin_audit(
                    &store,
                    "admin_grant_added",
                    "grant",
                    Some(grant.source_id.as_str()),
                    Some(grant.token_id.as_str()),
                )?;
                Ok(())
            }
            Some("revoke") => {
                let token_id = args.next().ok_or("grants revoke requires a token id")?;
                let source_id = args.next().ok_or("grants revoke requires a source id")?;
                let tool = Tool::parse(&args.next().ok_or("grants revoke requires a tool name")?)
                    .map_err(|error| error.to_string())?;
                store
                    .revoke_grant(&token_id, &source_id, tool, "admin-cli")
                    .map_err(|error| error.to_string())?;
                println!("revoked {} {}/{}", token_id, source_id, tool.as_str());
                Ok(())
            }
            Some(token_id) => {
                let grants = store
                    .list_grants_for_token(token_id)
                    .map_err(|error| error.to_string())?;
                if grants.is_empty() {
                    println!("no grants for token {token_id}");
                }
                for grant in grants {
                    println!(
                        "{}  {}  {}",
                        grant.source_id,
                        grant.tool.as_str(),
                        grant.policy.as_str()
                    );
                }
                Ok(())
            }
            None => Err("grants requires a token id or 'add'/'revoke'".to_string()),
        },
        "audit" => {
            let limit = match args.next() {
                None => 50,
                Some(value) => value
                    .parse::<usize>()
                    .map_err(|_| "audit limit must be a number".to_string())?,
            };
            let events = store
                .list_recent_audit(limit)
                .map_err(|error| error.to_string())?;
            if events.is_empty() {
                println!("no audit events");
            }
            for event in events {
                println!(
                    "{}  {}  actor={}  tool={}  source={}  {}  {}",
                    event.ts,
                    event.event,
                    event.actor,
                    event.tool,
                    event.source_id.as_deref().unwrap_or("-"),
                    event.decision,
                    event.reason_code.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        "approvals" => {
            let limit = match args.next() {
                None => 20,
                Some(value) => value
                    .parse::<usize>()
                    .map_err(|_| "approvals limit must be a number".to_string())?,
            };
            let pending = store
                .list_pending_approvals(limit)
                .map_err(|error| error.to_string())?;
            if pending.is_empty() {
                println!("no pending approvals");
            }
            for record in pending {
                println!(
                    "{}  {}  source={}  created_at={}",
                    record.id,
                    record.draft.tool.as_str(),
                    record.draft.source_id,
                    record.created_at
                );
            }
            Ok(())
        }
        "approve" | "deny" => {
            let id = parse_id(command, args.next())?;
            let outcome = store
                .decide_approval(
                    id,
                    command == "approve",
                    "admin-cli",
                    now,
                    config.approval_execution_ttl_ms,
                )
                .map_err(|error| error.to_string())?;
            println!("{command} {id}: {outcome:?}");
            Ok(())
        }
        "archive-source" => {
            let id = args.next().ok_or("archive-source requires a source id")?;
            let source = store
                .archive_source(&id, "admin-cli", now)
                .map_err(|error| error.to_string())?;
            println!("archived {} (physical database untouched)", source.id);
            Ok(())
        }
        "restore-source" => {
            let id = args.next().ok_or("restore-source requires a source id")?;
            let source = store
                .restore_source(&id, &config.provision_limits, "admin-cli", now)
                .map_err(|error| error.to_string())?;
            println!("restored {}", source.id);
            Ok(())
        }
        "reservations" => {
            let reservations = store
                .list_recent_reservations(20)
                .map_err(|error| error.to_string())?;
            if reservations.is_empty() {
                println!("no provisioning reservations");
            }
            for reservation in reservations {
                println!(
                    "{}  {}  parent={}  database={}  {}  {}",
                    reservation.id,
                    reservation.token_id,
                    reservation.parent_source_id,
                    reservation.database_name,
                    reservation.status.as_str(),
                    reservation.reason_code.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        "settle-reservation" => {
            let id = parse_id(command, args.next())?;
            let target = match args.next().as_deref() {
                Some("release") => mcp_db::ReservationStatus::Released,
                Some("commit") => mcp_db::ReservationStatus::Committed,
                _ => return Err("settle-reservation requires release|commit".to_string()),
            };
            let reservation = store
                .settle_reservation(id, target, Some("ADMIN_SETTLED"), now)
                .map_err(|error| error.to_string())?;
            println!("reservation {id}: {}", reservation.status.as_str());
            Ok(())
        }
        _ => Err(format!("unknown admin command: {command}")),
    }
}

/// Records one admin CLI change. Only identifiers are written; tokens, hashes,
/// DSNs and passwords never enter the audit row.
fn record_admin_audit(
    store: &Store,
    event: &str,
    tool: &str,
    source_id: Option<&str>,
    token_id: Option<&str>,
) -> Result<(), String> {
    let event = AuditEvent {
        ts: now_ms(),
        event: event.to_string(),
        approval_id: None,
        token_id: token_id.map(str::to_string),
        agent: None,
        source_id: source_id.map(str::to_string),
        tool: tool.to_string(),
        profile_id: None,
        profile_version: None,
        route_id: None,
        route_version: None,
        request_digest: None,
        channel: None,
        decision: "allowed".to_string(),
        actor: "admin-cli".to_string(),
        high_risk: false,
        reason_code: None,
        reason: None,
        duration_ms: None,
        row_count: None,
        error_code: None,
    };
    store
        .record_audit_event(&event)
        .map_err(|error| error.to_string())
}

fn default_policy(tool: Tool) -> ApprovalPolicy {
    if tool.is_write() || tool == Tool::CreateDatabase {
        ApprovalPolicy::Approval
    } else {
        ApprovalPolicy::Direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_db::auth::hash_token;
    use mcp_db::{CredentialResolver, CredentialSink};
    use std::fs;

    #[test]
    fn unknown_argument_is_rejected() {
        let result = run(["--nope".to_string()].into_iter());
        assert!(result.is_err());
    }

    #[test]
    fn version_and_help_succeed() {
        assert!(run(["--version".to_string()].into_iter()).is_ok());
        assert!(run(["--help".to_string()].into_iter()).is_ok());
    }

    #[test]
    fn credential_resolver_is_created_from_a_clean_directory() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = dir.path().join("credentials.enc");
        let key = dir.path().join("master.key");
        fs::write(&key, "07".repeat(32)).expect("write key");

        let resolver = open_credential_resolver(&store, &key).expect("open or create");
        assert!(store.is_file());
        assert!(matches!(
            resolver.resolve("keyring:mcp-db/test/query"),
            Err(mcp_db::Error::CredentialUnavailable)
        ));

        let reference = "keyring:mcp-db/test/query";
        let dsn = "postgres://reader:not-a-real-password@db.example:5432/app";
        resolver.store(reference, dsn).expect("store credential");
        assert_eq!(resolver.resolve(reference).expect("resolve"), dsn);

        let reopened = open_credential_resolver(&store, &key).expect("reopen");
        assert_eq!(reopened.resolve(reference).expect("resolve"), dsn);
    }

    #[test]
    fn missing_or_invalid_master_key_fails_credential_init() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = dir.path().join("credentials.enc");
        let missing = dir.path().join("missing.key");
        let error = open_credential_resolver(&store, &missing)
            .err()
            .expect("missing key must fail");
        assert!(error.starts_with("credential init failed: "), "{error}");
        assert!(!store.exists(), "failed init must not create a store");

        let invalid = dir.path().join("invalid.key");
        fs::write(&invalid, "not-a-master-key").expect("write invalid key");
        let error = open_credential_resolver(&store, &invalid)
            .err()
            .expect("invalid key must fail");
        assert!(error.starts_with("credential init failed: "), "{error}");
        assert!(!store.exists(), "failed init must not create a store");
    }

    #[test]
    fn admin_cli_changes_are_audited_without_secrets() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = Store::open(dir.path().join("state.db")).expect("open store");
        let issued = store.create_token("agent-a").expect("token");
        let token_id = issued.token_id.as_str();
        for (event, tool, source_id, token) in [
            ("admin_source_added", "source", Some("a"), None),
            ("admin_profile_added", "profile", Some("a"), None),
            ("admin_route_added", "route", Some("a"), None),
            ("admin_token_issued", "token", None, Some(token_id)),
            ("admin_token_revoked", "token", None, Some(token_id)),
            ("admin_grant_added", "grant", Some("a"), Some(token_id)),
            ("admin_grant_revoked", "grant", Some("a"), Some(token_id)),
        ] {
            record_admin_audit(&store, event, tool, source_id, token).expect("audit event");
        }

        let events = store.list_recent_audit(10).expect("recent audit");
        assert_eq!(events.len(), 7);
        let names = events
            .iter()
            .map(|event| event.event.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "admin_source_added",
            "admin_profile_added",
            "admin_route_added",
            "admin_token_issued",
            "admin_token_revoked",
            "admin_grant_added",
            "admin_grant_revoked",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        for event in &events {
            assert_eq!(event.actor, "admin-cli");
            assert_eq!(event.decision, "allowed");
        }
        let rendered = format!("{events:?}");
        assert!(!rendered.contains(&issued.token));
        assert!(!rendered.contains(&hash_token(&issued.token)));
    }
}
