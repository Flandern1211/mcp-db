pub mod auth;
pub mod authorization;
pub mod config;
pub mod error;
pub mod mcp;
pub mod state;
pub mod transport;

pub use authorization::{AuthorizedCall, Authorizer};
pub use config::Config;
pub use error::Error;
pub use mcp::{AppState, McpDbServer};
pub use state::{
    ApprovalPolicy, Driver, Environment, Grant, IssuedToken, NewGrant, NewProfile, NewRoute,
    NewSource, Profile, ProfilePurpose, Route, Source, SourceStatus, Store, TokenIdentity, Tool,
};

use std::io::{Error as IoError, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub const DEFAULT_ADDR: &str = "127.0.0.1:8787";
pub const HEALTHCHECK_ADDR: &str = "127.0.0.1:8787";

pub fn health_body() -> &'static str {
    r#"{"status":"ok","service":"mcp-db"}"#
}

/// Serve the skeleton health endpoint. Business routes replace this in later tasks.
pub fn serve(listener: TcpListener) -> std::io::Result<()> {
    for incoming in listener.incoming() {
        let mut stream = incoming?;
        let _ = handle(&mut stream);
    }
    Ok(())
}

pub fn check_health(addr: &str) -> std::io::Result<()> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if response.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(IoError::other(format!(
            "unexpected health response: {}",
            first_line(&response)
        )))
    }
}

fn handle(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buffer = [0_u8; 1024];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    let (status, body) = match path {
        "/healthz" => ("200 OK", health_body()),
        _ => ("404 Not Found", r#"{"error":"not found"}"#),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn start_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener address").to_string();
        thread::spawn(move || serve(listener));
        addr
    }

    fn get(addr: &str, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect test server");
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }

    #[test]
    fn health_endpoint_returns_ok() {
        let addr = start_server();
        let response = get(&addr, "/healthz");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with(health_body()));
    }

    #[test]
    fn unknown_path_returns_not_found() {
        let addr = start_server();
        let response = get(&addr, "/");
        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
    }

    #[test]
    fn healthcheck_accepts_a_live_server() {
        let addr = start_server();
        check_health(&addr).expect("healthcheck should pass");
    }
}
