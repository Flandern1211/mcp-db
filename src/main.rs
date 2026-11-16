use std::env;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use mcp_db::{
    check_health, Config, EncryptedFileResolver, MetadataProvider, PostgresMetadata, Store,
    UnavailableMetadata, DEFAULT_ADDR, HEALTHCHECK_ADDR,
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
        Some(other) => Err(format!("unknown argument: {other}")),
        None => {
            let config = Config::from_env().map_err(|error| error.to_string())?;
            let store = Store::open(&config.state_path)
                .map_err(|error| format!("state init failed: {error}"))?;
            let store = Arc::new(Mutex::new(store));
            let metadata: Arc<dyn MetadataProvider> =
                match (&config.credential_store, &config.master_key_file) {
                    (Some(store_path), Some(key_path)) => {
                        let resolver = EncryptedFileResolver::open(store_path, key_path)
                            .map_err(|error| format!("credential init failed: {error}"))?;
                        Arc::new(PostgresMetadata::new(Arc::new(resolver)))
                    }
                    _ => Arc::new(UnavailableMetadata),
                };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("runtime init failed: {error}"))?;
            runtime
                .block_on(mcp_db::transport::http::run(config, store, metadata))
                .map_err(|error| format!("server error: {error}"))
        }
    }
}

fn print_help() {
    println!(
        "mcp-db {}\n\nUsage:\n  mcp-db                 Start the MCP service (MCPDB_ADDR, default {DEFAULT_ADDR})\n  mcp-db --healthcheck   Check a running service (MCPDB_HEALTHCHECK_ADDR)\n  mcp-db --version\n  mcp-db --help",
        env!("CARGO_PKG_VERSION")
    );
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
