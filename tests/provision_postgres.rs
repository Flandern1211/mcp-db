//! Run only against an explicitly authorized PostgreSQL instance:
//! `cargo test --test provision_postgres -- --ignored`
//!
//! Set MCPDB_TEST_PG_ADMIN_DSN to a non-superuser account with CREATEDB and CREATEROLE.
//! The test creates one database and two roles, registers the new source and removes
//! them again at the end. Do not point it at a shared or production instance.

use std::collections::BTreeMap;
use std::fs;

use mcp_db::provision::{credential_ref, result_source};
use mcp_db::{
    CredentialResolver, Driver, EncryptedFileResolver, Environment, MetadataProvider, NewSource,
    PostgresExecutor, PostgresMetadata, PostgresProvisioner, ProfilePurpose, ProvisionLimits,
    ProvisionSpec, Provisioner, ReservationStatus, SqlExecutor, Store, Tool,
};
use tempfile::TempDir;
use tokio_postgres::{error::SqlState, Client, NoTls};

fn admin_dsn() -> String {
    std::env::var("MCPDB_TEST_PG_ADMIN_DSN")
        .ok()
        .filter(|dsn| !dsn.trim().is_empty())
        .expect("set MCPDB_TEST_PG_ADMIN_DSN to a CREATEDB/CREATEROLE test account")
}

fn key_hex(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn connect(dsn: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(dsn, NoTls)
        .await
        .unwrap_or_else(|_| panic!("test connection failed"));
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn cleanup(dsn: &str, database: &str, read_only_role: &str, read_write_role: &str) {
    let client = connect(dsn).await;
    let _ = client
        .batch_execute(&format!("DROP DATABASE IF EXISTS \"{database}\""))
        .await;
    for role in [read_only_role, read_write_role] {
        let _ = client
            .batch_execute(&format!("DROP ROLE IF EXISTS \"{role}\""))
            .await;
    }
}

#[tokio::test]
#[ignore = "requires MCPDB_TEST_PG_ADMIN_DSN with CREATEDB and CREATEROLE"]
async fn provisioned_database_is_read_only_usable_and_registered() {
    let dsn = admin_dsn();
    let dir = TempDir::new().expect("temp dir");
    let store_path = dir.path().join("credentials.enc");
    let key_path = dir.path().join("master.key");
    let key = [11_u8; 32];
    fs::write(&key_path, key_hex(&key)).expect("write key");
    let entries = BTreeMap::from([("keyring:provision/test".to_string(), dsn.clone())]);
    EncryptedFileResolver::seal(&store_path, &key, &entries).expect("seal");
    let resolver =
        std::sync::Arc::new(EncryptedFileResolver::open(&store_path, &key_path).expect("open"));
    let provisioner = PostgresProvisioner::new(resolver.clone(), resolver.clone());

    let limits = ProvisionLimits::new(
        5,
        1,
        vec![
            Tool::ListTables,
            Tool::QuerySql,
            Tool::MutateSql,
            Tool::ExecuteDdl,
        ],
    )
    .unwrap();
    let spec = ProvisionSpec::generate(Environment::NonProduction, &limits, Some("it")).unwrap();
    let profile = mcp_db::Profile {
        id: 1,
        source_id: "parent".to_string(),
        name: "provision".to_string(),
        purpose: ProfilePurpose::Provision,
        credential_ref: "keyring:provision/test".to_string(),
        enabled: true,
        version: 1,
        max_connections: None,
        statement_timeout_ms: None,
    };

    let outcome = provisioner.create(&profile, &spec).await;
    assert!(outcome.is_ok(), "provisioning must succeed");
    assert_eq!(outcome.unwrap().database, spec.database);

    let read_only_dsn = resolver
        .resolve(&credential_ref(&spec.database, ProfilePurpose::Query))
        .expect("read-only credential");
    let read_write_dsn = resolver
        .resolve(&credential_ref(&spec.database, ProfilePurpose::Mutate))
        .expect("read-write credential");
    let read_only = connect(&read_only_dsn).await;
    let read_write = connect(&read_write_dsn).await;

    read_write
        .batch_execute("CREATE TABLE public.items (id bigint PRIMARY KEY, name text)")
        .await
        .expect("read-write role creates schema objects");
    read_write
        .batch_execute("INSERT INTO public.items (id, name) VALUES (1, 'first')")
        .await
        .expect("read-write role inserts");

    let count: i64 = read_only
        .query_one("SELECT COUNT(*) FROM public.items", &[])
        .await
        .expect("read-only role selects by default privilege")
        .get(0);
    assert_eq!(count, 1);
    let insert = read_only
        .batch_execute("INSERT INTO public.items (id, name) VALUES (2, 'second')")
        .await
        .expect_err("read-only role must not insert");
    assert_eq!(insert.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
    let ddl = read_only
        .batch_execute("CREATE TABLE public.probe (id bigint)")
        .await
        .expect_err("read-only role must not create tables");
    assert_eq!(ddl.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));

    // Registration wires the new profiles and grants into the state store.
    let store = Store::open(dir.path().join("state.db")).expect("open store");
    store
        .create_source(&NewSource {
            id: "parent".to_string(),
            driver: Driver::Postgres,
            environment: Environment::NonProduction,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: None,
        })
        .unwrap();
    let token = store.create_token("provision-test").unwrap().token_id;
    let reservation = store
        .reserve_provision(
            &token,
            "parent",
            &spec.database,
            None,
            &limits,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
        )
        .unwrap();
    let identity = mcp_db::TokenIdentity {
        token_id: token.clone(),
        agent: "provision-test".to_string(),
    };
    let input = result_source(&spec, "parent", &identity).unwrap();
    let source = store
        .register_provisioned_source(&input, reservation.id, 1)
        .unwrap();
    assert_eq!(source.id, spec.database);
    assert_eq!(
        store.get_reservation(reservation.id).unwrap().status,
        ReservationStatus::Committed
    );

    let query_profile = store
        .list_profiles(&source.id)
        .unwrap()
        .into_iter()
        .find(|profile| profile.purpose == ProfilePurpose::Query)
        .expect("query profile");
    let connections = std::sync::Arc::new(mcp_db::postgres::ProfileConnections::default());
    let executor = PostgresExecutor::with_connections(resolver.clone(), connections.clone());
    let metadata = PostgresMetadata::with_connections(resolver.clone(), connections);
    let rows = executor
        .query(&query_profile, "SELECT name FROM public.items", &[], 10)
        .await
        .expect("registered query profile works");
    assert_eq!(rows.rows.len(), 1);
    let tables = metadata
        .list_tables(&query_profile, Some("public"))
        .await
        .expect("registered metadata works");
    assert!(tables
        .as_array()
        .expect("table list")
        .iter()
        .any(|table| table.get("name").and_then(|value| value.as_str()) == Some("items")));

    drop(read_only);
    drop(read_write);
    cleanup(
        &dsn,
        &spec.database,
        &spec.read_only_role,
        &spec.read_write_role,
    )
    .await;
}
