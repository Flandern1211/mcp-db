use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use mcp_db::{CredentialResolver, EncryptedFileResolver, Error};
use tempfile::TempDir;

fn entries() -> BTreeMap<String, String> {
    BTreeMap::from([(
        "keyring:mcp-db/test/query".to_string(),
        "host=db.example dbname=app user=reader password=not-a-real-password".to_string(),
    )])
}

fn key_hex(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
#[ignore = "writes an encrypted fixture for container smoke tests"]
fn seal_container_fixture() {
    let dir = PathBuf::from(std::env::var("MCPDB_SEAL_DIR").expect("MCPDB_SEAL_DIR must be set"));
    let dsn = std::env::var("MCPDB_TEST_PG_DSN")
        .expect("MCPDB_TEST_PG_DSN must point at a controlled PostgreSQL test database");
    fs::create_dir_all(&dir).expect("create seal dir");
    let key = [9_u8; 32];
    fs::write(dir.join("master.key"), key_hex(&key)).expect("write key");
    let entries = BTreeMap::from([("keyring:mcp-db/test/query".to_string(), dsn)]);
    EncryptedFileResolver::seal(&dir.join("credentials.enc"), &key, &entries).expect("seal");
}

#[test]
#[ignore = "opens a credential fixture supplied by MCPDB_OPEN_DIR"]
fn open_container_fixture() {
    let dir = PathBuf::from(std::env::var("MCPDB_OPEN_DIR").expect("MCPDB_OPEN_DIR"));
    let resolver =
        EncryptedFileResolver::open(&dir.join("credentials.enc"), &dir.join("master.key"))
            .expect("open fixture");
    assert!(resolver.resolve("keyring:mcp-db/test/query").is_ok());
}

#[test]
fn encrypted_store_round_trips_without_plaintext() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key = [7_u8; 32];
    let key_file = dir.path().join("master.key");
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");

    let bytes = fs::read(&store).expect("read store");
    assert!(!bytes
        .windows(b"localhost".len())
        .any(|window| window == b"localhost"));
    assert!(!bytes
        .windows(b"not-a-real-password".len())
        .any(|window| window == b"not-a-real-password"));

    let resolver = EncryptedFileResolver::open(&store, &key_file).expect("open");
    let value = resolver
        .resolve("keyring:mcp-db/test/query")
        .expect("resolve");
    assert!(value.contains("host=db.example"));
    assert!(matches!(
        resolver.resolve("keyring:mcp-db/missing"),
        Err(Error::CredentialUnavailable)
    ));
}

#[test]
fn wrong_or_missing_key_fails_closed() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");

    fs::write(&key_file, key_hex(&[8_u8; 32])).expect("write wrong key");
    assert!(matches!(
        EncryptedFileResolver::open(&store, &key_file),
        Err(Error::CredentialUnavailable)
    ));

    fs::remove_file(&key_file).expect("remove key");
    assert!(matches!(
        EncryptedFileResolver::open(&store, &key_file),
        Err(Error::CredentialUnavailable)
    ));
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");

    let mut bytes = fs::read(&store).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&store, bytes).expect("tamper");

    assert!(matches!(
        EncryptedFileResolver::open(&store, &key_file),
        Err(Error::CredentialUnavailable)
    ));
}
