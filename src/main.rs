use std::env;
use std::net::TcpListener;
use std::process::ExitCode;

use mcp_db::{check_health, serve, Config, Store, DEFAULT_ADDR, HEALTHCHECK_ADDR};

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
            let _store = Store::open(&config.state_path)
                .map_err(|error| format!("state init failed: {error}"))?;
            let listener = TcpListener::bind(&config.addr)
                .map_err(|err| format!("cannot bind {}: {err}", config.addr))?;
            println!(
                "mcp-db {} listening on http://{} (skeleton: /healthz only)",
                env!("CARGO_PKG_VERSION"),
                config.addr
            );
            serve(listener).map_err(|err| format!("server error: {err}"))
        }
    }
}

fn print_help() {
    println!(
        "mcp-db {}\n\nUsage:\n  mcp-db                 Start the service (MCPDB_ADDR, default {DEFAULT_ADDR})\n  mcp-db --healthcheck   Check a running service (MCPDB_HEALTHCHECK_ADDR)\n  mcp-db --version\n  mcp-db --help",
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
