use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use mcp_db::{CredentialResolver, CredentialSink, EncryptedFileResolver, Error};
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
fn open_or_create_creates_an_empty_store_from_a_clean_directory() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    fs::write(&key_file, key_hex(&[7_u8; 32])).expect("write key");
    assert!(!store.exists());

    let resolver =
        EncryptedFileResolver::open_or_create(&store, &key_file).expect("open or create");
    assert!(store.is_file());
    assert!(matches!(
        resolver.resolve("keyring:mcp-db/test/query"),
        Err(Error::CredentialUnavailable)
    ));

    let reference = "keyring:provision/mcpdb_np_test/query";
    let dsn = "postgres://mcpdb_np_test_ro:s3cret-pw@db.example:5432/mcpdb_np_test";
    resolver.store(reference, dsn).expect("store credential");
    assert_eq!(resolver.resolve(reference).expect("resolve"), dsn);

    let reopened = EncryptedFileResolver::open(&store, &key_file).expect("reopen");
    assert_eq!(reopened.resolve(reference).expect("resolve"), dsn);
}

#[test]
fn open_or_create_still_requires_a_valid_master_key() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");

    assert!(matches!(
        EncryptedFileResolver::open_or_create(&store, &key_file),
        Err(Error::CredentialUnavailable)
    ));
    assert!(!store.exists(), "missing key must not create a store");

    fs::write(&key_file, "not-a-master-key").expect("write invalid key");
    assert!(matches!(
        EncryptedFileResolver::open_or_create(&store, &key_file),
        Err(Error::CredentialUnavailable)
    ));
    assert!(!store.exists(), "invalid key must not create a store");

    EncryptedFileResolver::seal(&store, &[7_u8; 32], &entries()).expect("seal");
    fs::write(&key_file, key_hex(&[8_u8; 32])).expect("write wrong key");
    assert!(matches!(
        EncryptedFileResolver::open_or_create(&store, &key_file),
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

#[test]
fn upsert_persists_new_references_and_hot_loads_them() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");

    let resolver = EncryptedFileResolver::open(&store, &key_file).expect("open");
    let reference = "keyring:provision/mcpdb_np_test/query";
    let dsn = "postgres://mcpdb_np_test_ro:s3cret-pw@db.example:5432/mcpdb_np_test";
    resolver.store(reference, dsn).expect("store credential");
    assert_eq!(resolver.resolve(reference).expect("resolve"), dsn);
    assert!(resolver.resolve("keyring:mcp-db/test/query").is_ok());

    let on_disk = fs::read(&store).expect("read store");
    assert!(!on_disk
        .windows(b"s3cret-pw".len())
        .any(|window| window == b"s3cret-pw"));

    let reopened = EncryptedFileResolver::open(&store, &key_file).expect("reopen");
    assert_eq!(reopened.resolve(reference).expect("resolve"), dsn);
    assert!(reopened.resolve("keyring:mcp-db/test/query").is_ok());
}

#[test]
fn upsert_rejects_invalid_references_and_multiline_values() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");
    let resolver = EncryptedFileResolver::open(&store, &key_file).expect("open");

    assert!(matches!(
        resolver.store("postgres://user:pw@host/db", "host=db"),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        resolver.store("keyring:provision/x", "postgres://a\nb"),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        resolver.store("keyring:provision/x", "   "),
        Err(Error::InvalidInput(_))
    ));
    assert!(resolver.resolve("keyring:provision/x").is_err());
}

#[test]
fn remove_deletes_one_reference_and_keeps_the_rest() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");
    let resolver = EncryptedFileResolver::open(&store, &key_file).expect("open");

    let extra = "keyring:provision/mcpdb_np_test/query";
    let dsn = "postgres://mcpdb_np_test_ro:s3cret-pw@db.example:5432/mcpdb_np_test";
    resolver.store(extra, dsn).expect("store credential");
    resolver
        .remove("keyring:mcp-db/test/query")
        .expect("remove");
    assert!(matches!(
        resolver.resolve("keyring:mcp-db/test/query"),
        Err(Error::CredentialUnavailable)
    ));
    assert_eq!(resolver.resolve(extra).expect("resolve"), dsn);

    // The removal survives a reopen: it is persisted, not just dropped in memory.
    let reopened = EncryptedFileResolver::open(&store, &key_file).expect("reopen");
    assert!(matches!(
        reopened.resolve("keyring:mcp-db/test/query"),
        Err(Error::CredentialUnavailable)
    ));
    assert_eq!(reopened.resolve(extra).expect("resolve"), dsn);
}

#[test]
fn removing_an_unknown_reference_is_idempotent() {
    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");
    let resolver = EncryptedFileResolver::open(&store, &key_file).expect("open");

    resolver
        .remove("keyring:mcp-db/never-existed")
        .expect("missing reference removal is a no-op");
    assert!(
        resolver.resolve("keyring:mcp-db/test/query").is_ok(),
        "removing an unknown reference must not touch other entries"
    );
}

#[test]
fn concurrent_reads_see_either_the_old_or_new_store() {
    use std::sync::Arc;

    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("credentials.enc");
    let key_file = dir.path().join("master.key");
    let key = [7_u8; 32];
    fs::write(&key_file, key_hex(&key)).expect("write key");
    EncryptedFileResolver::seal(&store, &key, &entries()).expect("seal");
    let resolver = Arc::new(EncryptedFileResolver::open(&store, &key_file).expect("open"));

    let reader = resolver.clone();
    let handle = std::thread::spawn(move || {
        for _ in 0..200 {
            reader
                .resolve("keyring:mcp-db/test/query")
                .expect("existing entry stays readable");
        }
    });
    for index in 0..10 {
        resolver
            .store(
                &format!("keyring:provision/db{index}/query"),
                "postgres://role:pw@db.example:5432/test",
            )
            .expect("store");
    }
    handle.join().expect("reader");
    for index in 0..10 {
        assert!(resolver
            .resolve(&format!("keyring:provision/db{index}/query"))
            .is_ok());
    }
}
