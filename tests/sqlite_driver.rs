use std::path::{Path, PathBuf};
use std::sync::Arc;

use mcp_db::postgres::ProfileConnections;
use mcp_db::{
    validate_for_driver_tool, Authorizer, CredentialResolver, Driver, Environment, Error,
    MetadataProvider, NewGrant, NewSource, Profile, ProfilePurpose, SqlExecutor, SqliteExecutor,
    SqliteMetadata, Store, Tool,
};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde_json::json;
use tempfile::TempDir;

struct PathResolver(PathBuf);

impl CredentialResolver for PathResolver {
    fn resolve(&self, _reference: &str) -> Result<String, Error> {
        Ok(self.0.to_string_lossy().into_owned())
    }
}

fn profile(source_id: &str, id: i64, purpose: ProfilePurpose, timeout_ms: u64) -> Profile {
    Profile {
        id,
        source_id: source_id.to_string(),
        name: purpose.as_str().to_string(),
        purpose,
        credential_ref: format!("keyring:mcp-db/test/{source_id}"),
        enabled: true,
        version: 1,
        max_connections: Some(2),
        statement_timeout_ms: Some(timeout_ms),
    }
}

fn fixture() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("fixture.db");
    let connection = Connection::open(&path).expect("create fixture");
    connection
        .execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO items (id, name) VALUES (1, 'alpha'), (2, 'beta');
             CREATE INDEX items_name_idx ON items (name);
             CREATE TABLE orders (
               id INTEGER PRIMARY KEY,
               item_id INTEGER NOT NULL REFERENCES items(id)
             );
             INSERT INTO orders (id, item_id) VALUES (10, 1);
             CREATE VIEW item_names AS SELECT name FROM items;",
        )
        .expect("load fixture");
    (dir, path)
}

fn read_snapshot(path: &Path) -> (Vec<(i64, String)>, Vec<String>) {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open snapshot");
    let mut statement = connection
        .prepare("SELECT id, name FROM items ORDER BY id")
        .expect("prepare items");
    let mut rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query items")
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()
        .expect("read items");
    drop(statement);
    rows.sort();
    let mut statement = connection
        .prepare(
            "SELECT type || ':' || name FROM sqlite_master
              WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .expect("prepare schema");
    let mut schema = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query schema")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("read schema");
    drop(statement);
    schema.sort();
    (rows, schema)
}

fn executor(path: &Path) -> SqliteExecutor {
    SqliteExecutor::with_connections(
        Arc::new(PathResolver(path.to_path_buf())),
        Arc::new(ProfileConnections::default()),
    )
}

fn metadata(path: &Path) -> SqliteMetadata {
    SqliteMetadata::with_connections(
        Arc::new(PathResolver(path.to_path_buf())),
        Arc::new(ProfileConnections::default()),
    )
}

#[tokio::test]
async fn query_profile_reads_but_native_boundary_denies_dml_and_ddl() {
    let (_dir, path) = fixture();
    let executor = executor(&path);
    let profile = profile("sqlite-main", 1, ProfilePurpose::Query, 5_000);

    let outcome = executor
        .query(&profile, "SELECT id, name FROM items ORDER BY id", &[], 10)
        .await
        .expect("read query");
    assert_eq!(outcome.columns, vec!["id", "name"]);
    assert_eq!(outcome.row_count, 2);
    assert_eq!(outcome.returned_rows, 2);
    assert!(!outcome.truncated);
    assert_eq!(outcome.rows[0], json!({"id": 1, "name": "alpha"}));

    // SQLite itself rejects writes on a read-only connection, independent of the
    // application read-only check.
    let native = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("native read-only connection");
    native
        .execute_batch("PRAGMA query_only = ON")
        .expect("query_only");
    for sql in [
        "INSERT INTO items (id, name) VALUES (3, 'gamma')",
        "UPDATE items SET name = 'changed'",
        "DELETE FROM items WHERE 0",
        "CREATE TABLE readonly_probe (id INTEGER)",
        "DROP TABLE items",
        "ALTER TABLE items ADD COLUMN extra TEXT",
        "CREATE INDEX readonly_probe_idx ON items (name)",
    ] {
        let failure = native
            .execute_batch(sql)
            .expect_err("SQLite must deny writes on a query connection");
        assert!(
            matches!(
                failure,
                rusqlite::Error::SqliteFailure(code, _)
                    if matches!(code.code, ErrorCode::ReadOnly | ErrorCode::PermissionDenied)
            ),
            "{sql} must fail with a read-only/permission error: {failure:?}"
        );
    }

    let before = read_snapshot(&path);
    for sql in [
        "INSERT INTO items (id, name) VALUES (3, 'gamma')",
        "UPDATE items SET name = 'changed' WHERE 1 = 0",
        "DELETE FROM items WHERE 1 = 0",
        "CREATE TABLE readonly_probe (id INTEGER)",
        "DROP TABLE items",
        "ALTER TABLE items ADD COLUMN extra TEXT",
        "CREATE INDEX readonly_probe_idx ON items (name)",
    ] {
        let result = executor.query(&profile, sql, &[], 10).await;
        assert!(
            matches!(result, Err(Error::InvalidInput(_))),
            "{sql} must be rejected by the query profile: {result:?}"
        );
        assert_eq!(read_snapshot(&path), before, "fixture changed after {sql}");
    }
}

#[tokio::test]
async fn mutate_and_schema_profiles_can_write() {
    let (_dir, path) = fixture();
    let executor = executor(&path);

    let mutate = profile("sqlite-main", 2, ProfilePurpose::Mutate, 5_000);
    let outcome = executor
        .mutate(
            &mutate,
            "INSERT INTO items (id, name) VALUES (?, ?)",
            &[json!(3), json!("gamma")],
        )
        .await
        .expect("mutate insert");
    assert_eq!(outcome.row_count, 1);

    let schema = profile("sqlite-main", 3, ProfilePurpose::Schema, 5_000);
    let outcome = executor
        .mutate(
            &schema,
            "CREATE TABLE audit_log (id INTEGER PRIMARY KEY, note TEXT)",
            &[],
        )
        .await
        .expect("schema create");
    assert_eq!(outcome.row_count, 0);

    let (rows, tables) = read_snapshot(&path);
    assert_eq!(
        rows,
        vec![
            (1, "alpha".to_string()),
            (2, "beta".to_string()),
            (3, "gamma".to_string())
        ]
    );
    assert!(tables.contains(&"table:audit_log".to_string()));
}

#[test]
fn sqlite_dialect_rejects_multi_statement_and_administrative_sql() {
    for sql in [
        "SELECT 1; SELECT 2",
        "ATTACH DATABASE 'other.db' AS other",
        "DETACH DATABASE other",
        "PRAGMA query_only = OFF",
        "PRAGMA writable_schema = ON",
        "VACUUM",
        "VACUUM INTO 'copy.db'",
        "REINDEX items",
        "ANALYZE",
        "SELECT load_extension('evil')",
        "CREATE TRIGGER items_trigger AFTER INSERT ON items BEGIN SELECT 1; END",
        "CREATE VIRTUAL TABLE docs USING fts5(body)",
        "TRUNCATE TABLE items",
        "EXPLAIN ANALYZE SELECT 1",
        "EXPLAIN QUERY PLAN SELECT 1",
        "CREATE DATABASE app",
        "GRANT SELECT ON items TO reader",
    ] {
        // Every tool refuses the statement before it reaches any database file.
        let mut rejected = false;
        for tool in [
            Tool::QuerySql,
            Tool::MutateSql,
            Tool::ExecuteDdl,
            Tool::ExplainQuery,
        ] {
            if validate_for_driver_tool(sql, tool, Driver::Sqlite).is_err() {
                rejected = true;
            }
        }
        assert!(rejected, "{sql} must be rejected");
    }

    let query =
        validate_for_driver_tool("SELECT * FROM items", Tool::QuerySql, Driver::Sqlite).unwrap();
    assert_eq!(query.driver, Driver::Sqlite);
    assert_eq!(
        query.explain_sql(),
        "EXPLAIN QUERY PLAN SELECT * FROM items"
    );

    let insert = validate_for_driver_tool(
        "INSERT INTO items (id, name) VALUES (3, 'gamma')",
        Tool::MutateSql,
        Driver::Sqlite,
    )
    .unwrap();
    assert_eq!(insert.class, mcp_db::SqlClass::Mutate);

    let ddl = validate_for_driver_tool(
        "CREATE TABLE audit_log (id INTEGER PRIMARY KEY)",
        Tool::ExecuteDdl,
        Driver::Sqlite,
    )
    .unwrap();
    assert_eq!(ddl.class, mcp_db::SqlClass::Ddl);
}

#[tokio::test]
async fn max_rows_truncates_like_postgres() {
    let (_dir, path) = fixture();
    let executor = executor(&path);
    let profile = profile("sqlite-main", 1, ProfilePurpose::Query, 5_000);
    let outcome = executor
        .query(&profile, "SELECT id FROM items ORDER BY id", &[], 1)
        .await
        .expect("limited query");
    assert_eq!(outcome.row_count, 2);
    assert_eq!(outcome.returned_rows, 1);
    assert!(outcome.truncated);
    assert_eq!(outcome.rows.len(), 1);
}

#[tokio::test]
async fn statement_timeout_interrupts_and_connection_recovers() {
    let (_dir, path) = fixture();
    let executor = executor(&path);
    let profile = profile("sqlite-main", 1, ProfilePurpose::Query, 50);

    let result = executor
        .query(
            &profile,
            "WITH RECURSIVE counter(x) AS (
               SELECT 1 UNION ALL SELECT x + 1 FROM counter
             ) SELECT count(*) FROM counter",
            &[],
            10,
        )
        .await;
    assert!(
        matches!(result, Err(Error::ResourceLimit(_))),
        "long query must hit the statement timeout: {result:?}"
    );

    let outcome = executor
        .query(&profile, "SELECT 1 AS value", &[], 10)
        .await
        .expect("connection recovers after interruption");
    assert_eq!(outcome.rows[0], json!({"value": 1}));
}

#[tokio::test]
async fn metadata_lists_main_schema_and_describes_tables() {
    let (_dir, path) = fixture();
    let metadata = metadata(&path);
    let profile = profile("sqlite-main", 1, ProfilePurpose::Query, 5_000);

    let tables = metadata.list_tables(&profile, None).await.unwrap();
    let names = tables
        .as_array()
        .unwrap()
        .iter()
        .map(|table| table["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["item_names", "items", "orders"]);
    for table in tables.as_array().unwrap() {
        assert_eq!(table["schema"], "main");
        assert!(table["kind"] == "VIEW" || table["kind"] == "BASE TABLE");
    }
    assert_eq!(
        metadata.list_tables(&profile, Some("main")).await.unwrap(),
        tables
    );
    assert_eq!(
        metadata.list_tables(&profile, Some("other")).await.unwrap(),
        json!([])
    );

    let items = metadata
        .describe_table(&profile, "items", None)
        .await
        .unwrap();
    assert_eq!(items["schema"], "main");
    assert_eq!(items["table"], "items");
    assert_eq!(items["columns"][0]["name"], "id");
    assert_eq!(items["columns"][0]["type"], "INTEGER");
    assert_eq!(items["columns"][0]["nullable"], false);
    assert_eq!(items["columns"][1]["name"], "name");
    assert_eq!(items["primary_key"], json!(["id"]));
    assert!(items["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|index| index["name"] == "items_name_idx"
            && index["definition"]
                .as_str()
                .unwrap()
                .contains("CREATE INDEX")));
    assert_eq!(items["foreign_keys"], json!([]));

    let orders = metadata
        .describe_table(&profile, "orders", None)
        .await
        .unwrap();
    assert_eq!(orders["foreign_keys"][0]["references_schema"], "main");
    assert_eq!(orders["foreign_keys"][0]["references_table"], "items");
    assert_eq!(orders["foreign_keys"][0]["column"], "item_id");
    assert_eq!(orders["foreign_keys"][0]["references_column"], "id");

    let view = metadata
        .describe_table(&profile, "item_names", None)
        .await
        .unwrap();
    assert_eq!(view["columns"][0]["name"], "name");
}

#[test]
fn sqlite_source_and_driver_survive_restart_and_discovery() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("state.db");
    let store = Store::open(&path).expect("open state");
    assert_eq!(store.schema_version().unwrap(), 6);

    let issued = store.create_token("agent-sqlite").expect("token");
    let source = store
        .create_source(&NewSource {
            id: "sqlite-main".to_string(),
            driver: Driver::Sqlite,
            environment: Environment::NonProduction,
            parent_source_id: None,
            created_token_id: None,
            created_agent: None,
            created_origin: None,
        })
        .expect("create sqlite source");
    assert_eq!(source.driver, Driver::Sqlite);
    store
        .create_grant(&NewGrant {
            token_id: issued.token_id.clone(),
            source_id: "sqlite-main".to_string(),
            tool: Tool::QuerySql,
            policy: None,
        })
        .expect("grant query");
    let visible = Authorizer::new(&store)
        .visible_sources(&issued.token_id)
        .expect("visible sources");
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].driver, Driver::Sqlite);
    drop(store);

    let store = Store::open(&path).expect("reopen state");
    assert_eq!(
        store.get_source("sqlite-main").unwrap().driver,
        Driver::Sqlite
    );
    assert!(matches!(
        Driver::parse("mysql"),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        Driver::parse("mssql"),
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn invalid_sqlite_locations_are_rejected() {
    let executor = SqliteExecutor::new(Arc::new(PathResolver(PathBuf::from(":memory:"))));
    let profile = profile("sqlite-main", 1, ProfilePurpose::Query, 5_000);
    assert!(matches!(
        executor.query(&profile, "SELECT 1", &[], 1).await,
        Err(Error::InvalidInput(_))
    ));

    let empty = SqliteExecutor::new(Arc::new(PathResolver(PathBuf::new())));
    assert!(matches!(
        empty.query(&profile, "SELECT 1", &[], 1).await,
        Err(Error::InvalidInput(_))
    ));
}
