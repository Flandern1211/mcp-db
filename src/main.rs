use std::env;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use mcp_db::{
    check_health, Config, EncryptedFileResolver, MetadataProvider, PostgresExecutor,
    PostgresMetadata, SqlExecutor, Store, UnavailableExecutor, UnavailableMetadata, DEFAULT_ADDR,
    HEALTHCHECK_ADDR,
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
        Some(command)
            if matches!(
                command,
                "approvals"
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
            let store = Store::open(&config.state_path)
                .map_err(|error| format!("state init failed: {error}"))?;
            let store = Arc::new(Mutex::new(store));
            let metadata: Arc<dyn MetadataProvider>;
            let executor: Arc<dyn SqlExecutor>;
            let provisioner: Arc<dyn mcp_db::provision::Provisioner>;
            match (&config.credential_store, &config.master_key_file) {
                (Some(store_path), Some(key_path)) => {
                    let resolver = EncryptedFileResolver::open(store_path, key_path)
                        .map_err(|error| format!("credential init failed: {error}"))?;
                    let resolver = Arc::new(resolver);
                    let connections = Arc::new(mcp_db::postgres::ProfileConnections::default());
                    metadata = Arc::new(PostgresMetadata::with_connections(
                        resolver.clone(),
                        connections.clone(),
                    ));
                    executor = Arc::new(PostgresExecutor::with_connections(
                        resolver.clone(),
                        connections,
                    ));
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
                ))
                .map_err(|error| format!("server error: {error}"))
        }
    }
}

fn print_help() {
    println!(
        "mcp-db {}\n\nUsage:\n  mcp-db                 Start the MCP service (MCPDB_ADDR, default {DEFAULT_ADDR})\n  mcp-db --healthcheck   Check a running service (MCPDB_HEALTHCHECK_ADDR)\n  mcp-db --version\n  mcp-db --help\n\nLocal admin commands (use the same MCPDB_STATE_PATH as the service):\n  mcp-db approvals [limit]        List pending approvals\n  mcp-db approve <id>             Approve one pending request\n  mcp-db deny <id>                Deny one pending request\n  mcp-db archive-source <id>      Archive a source without dropping the database\n  mcp-db restore-source <id>      Restore an archived source after quota checks\n  mcp-db reservations             List recent provisioning reservations\n  mcp-db settle-reservation <id> release|commit",
        env!("CARGO_PKG_VERSION")
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
