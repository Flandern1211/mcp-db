use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params_from_iter, Connection, ErrorCode, OpenFlags, OptionalExtension};
use serde_json::{Number, Value};

use crate::credentials::CredentialResolver;
use crate::error::{Error, Result};
use crate::executor::{MutationOutcome, QueryOutcome, SqlExecutor};
use crate::metadata::MetadataProvider;
use crate::postgres::{ProfileConnections, DEFAULT_STATEMENT_TIMEOUT_MS};
use crate::state::{Profile, ProfilePurpose};

const SQLITE_SCHEMA: &str = "main";
const PROGRESS_INSTRUCTIONS: i32 = 1_000;

/// Opens one short-lived SQLite connection per call. Query profile connections are
/// opened read-only and verified natively before any caller statement runs.
struct SqliteConnection {
    resolver: Arc<dyn CredentialResolver>,
    connections: Arc<ProfileConnections>,
}

impl SqliteConnection {
    fn new(resolver: Arc<dyn CredentialResolver>, connections: Arc<ProfileConnections>) -> Self {
        Self {
            resolver,
            connections,
        }
    }

    async fn run<T, F>(&self, profile: &Profile, read_only: bool, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        // Admission happens before credential resolution or any file access.
        let permit = self.connections.acquire(profile)?;
        let path = resolve_database_path(self.resolver.as_ref(), profile)?;
        let timeout = profile
            .statement_timeout_ms
            .unwrap_or(DEFAULT_STATEMENT_TIMEOUT_MS);
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let connection = open_connection(&path, read_only)?;
            install_timeout(&connection, timeout)?;
            if read_only {
                verify_read_only(&connection)?;
            }
            work(&connection)
        })
        .await;
        result.unwrap_or_else(|_| {
            if read_only {
                Err(Error::MetadataUnavailable)
            } else {
                Err(Error::ExecutionUnknown)
            }
        })
    }
}

fn resolve_database_path(resolver: &dyn CredentialResolver, profile: &Profile) -> Result<PathBuf> {
    let raw = resolver.resolve(&profile.credential_ref)?;
    let path = raw.trim();
    if path.is_empty() || path == ":memory:" || path.contains(['\n', '\r']) {
        return Err(Error::InvalidInput(
            "SQLite credential must resolve to a database file path".to_string(),
        ));
    }
    Ok(PathBuf::from(path))
}

fn open_connection(path: &Path, read_only: bool) -> Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let connection = Connection::open_with_flags(path, flags).map_err(|_| {
        Error::ExecutionNotStarted("SQLite database file is unavailable".to_string())
    })?;
    if read_only {
        connection
            .execute_batch("PRAGMA query_only = ON")
            .map_err(|_| Error::InvalidInput("query profile is not read-only".to_string()))?;
    }
    Ok(connection)
}

fn install_timeout(connection: &Connection, timeout_ms: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    connection.progress_handler(
        PROGRESS_INSTRUCTIONS,
        Some(move || Instant::now() >= deadline),
    );
    Ok(())
}

/// A query profile must prove its read-only boundary with the database itself:
/// the open flag plus `query_only` setting, followed by real DML and DDL attempts
/// against an existing table and the main schema.
fn verify_read_only(connection: &Connection) -> Result<()> {
    let query_only: i64 = connection
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .map_err(query_error)?;
    if query_only != 1 {
        return Err(Error::InvalidInput(
            "query profile is not read-only".to_string(),
        ));
    }

    let table: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
              LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(query_error)?;
    let dml = match &table {
        Some(table) => format!("DELETE FROM {} WHERE 0", quote_identifier(table)),
        // An empty database has no table to touch; a write transaction still proves
        // the connection cannot begin writing.
        None => "BEGIN IMMEDIATE".to_string(),
    };
    match connection.execute_batch(&dml) {
        Err(rusqlite::Error::SqliteFailure(code, _))
            if matches!(code.code, ErrorCode::ReadOnly | ErrorCode::PermissionDenied) => {}
        other => {
            return Err(Error::InvalidInput(format!(
                "query profile failed native read-only DML verification: {other:?}"
            )))
        }
    }
    if table.is_none() {
        let _ = connection.execute_batch("ROLLBACK");
    }

    // The savepoint protects even an unexpected success on a writable file.
    let ddl = connection.execute_batch(
        "SAVEPOINT mcpdb_readonly_probe;
         CREATE TABLE mcpdb_readonly_probe (id INTEGER);",
    );
    let _ = connection.execute_batch(
        "ROLLBACK TO mcpdb_readonly_probe;
         RELEASE mcpdb_readonly_probe;",
    );
    match ddl {
        Err(rusqlite::Error::SqliteFailure(code, _))
            if matches!(
                code.code,
                ErrorCode::ReadOnly
                    | ErrorCode::PermissionDenied
                    | ErrorCode::AuthorizationForStatementDenied
            ) => {}
        other => {
            return Err(Error::InvalidInput(format!(
                "query profile failed native read-only DDL verification: {other:?}"
            )))
        }
    }
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn query_error(error: rusqlite::Error) -> Error {
    match sqlite_failure(&error) {
        Some(ErrorCode::OperationInterrupted) => {
            Error::ResourceLimit("SQLite statement timeout or interruption".to_string())
        }
        Some(ErrorCode::ReadOnly | ErrorCode::PermissionDenied) => {
            Error::InvalidInput("query profile is read-only".to_string())
        }
        Some(ErrorCode::ConstraintViolation) => Error::Conflict("constraint violation".to_string()),
        _ => Error::MetadataUnavailable,
    }
}

fn write_error(error: rusqlite::Error) -> Error {
    match sqlite_failure(&error) {
        Some(ErrorCode::OperationInterrupted) => {
            Error::ResourceLimit("SQLite statement timeout or interruption".to_string())
        }
        Some(ErrorCode::ConstraintViolation) => Error::Conflict("constraint violation".to_string()),
        _ => Error::MetadataUnavailable,
    }
}

fn sqlite_failure(error: &rusqlite::Error) -> Option<ErrorCode> {
    match error {
        rusqlite::Error::SqliteFailure(code, _) => Some(code.code),
        _ => None,
    }
}

fn json_to_sql(value: &Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(flag) => SqlValue::Integer(i64::from(*flag)),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                SqlValue::Integer(integer)
            } else {
                SqlValue::Real(number.as_f64().unwrap_or_default())
            }
        }
        Value::String(text) => SqlValue::Text(text.clone()),
        other => {
            return Err(Error::SqlRejected(format!(
                "parameter type {} is not supported",
                type_name(other)
            )))
        }
    })
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn row_to_json(row: &rusqlite::Row<'_>, names: &[String]) -> Value {
    let mut object = serde_json::Map::new();
    for (index, name) in names.iter().enumerate() {
        let value = match row.get_ref(index) {
            Ok(ValueRef::Null) => Value::Null,
            Ok(ValueRef::Integer(integer)) => Value::Number(integer.into()),
            Ok(ValueRef::Real(real)) => Number::from_f64(real)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Ok(ValueRef::Text(text)) => Value::String(String::from_utf8_lossy(text).into_owned()),
            Ok(ValueRef::Blob(bytes)) => Value::String(format!(
                "0x{}",
                bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )),
            Err(_) => Value::Null,
        };
        object.insert(name.clone(), value);
    }
    Value::Object(object)
}

pub struct SqliteExecutor {
    connection: SqliteConnection,
}

impl SqliteExecutor {
    pub fn new(resolver: Arc<dyn CredentialResolver>) -> Self {
        Self::with_connections(resolver, Arc::new(ProfileConnections::default()))
    }

    pub fn with_connections(
        resolver: Arc<dyn CredentialResolver>,
        connections: Arc<ProfileConnections>,
    ) -> Self {
        Self {
            connection: SqliteConnection::new(resolver, connections),
        }
    }

    async fn run_query(
        &self,
        profile: &Profile,
        sql: String,
        params: Vec<SqlValue>,
        max_rows: usize,
    ) -> Result<QueryOutcome> {
        self.connection
            .run(profile, true, move |connection| {
                let mut statement = connection.prepare(&sql).map_err(query_error)?;
                if !statement.readonly() {
                    return Err(Error::InvalidInput(
                        "query profile only accepts read-only statements".to_string(),
                    ));
                }
                let names = statement
                    .column_names()
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let mut rows = statement
                    .query(params_from_iter(params))
                    .map_err(query_error)?;
                let mut collected = Vec::new();
                let mut row_count = 0_usize;
                while let Some(row) = rows.next().map_err(query_error)? {
                    row_count += 1;
                    if collected.len() < max_rows {
                        collected.push(row_to_json(row, &names));
                    }
                }
                let returned_rows = row_count.min(max_rows);
                let columns = if collected.is_empty() {
                    Vec::new()
                } else {
                    names.clone()
                };
                Ok(QueryOutcome {
                    columns,
                    rows: collected,
                    row_count,
                    returned_rows,
                    truncated: row_count > max_rows,
                })
            })
            .await
    }
}

#[async_trait]
impl SqlExecutor for SqliteExecutor {
    async fn query(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<QueryOutcome> {
        if profile.purpose != ProfilePurpose::Query {
            return Err(Error::InvalidInput(
                "query tool requires a query profile".to_string(),
            ));
        }
        let params = params.iter().map(json_to_sql).collect::<Result<Vec<_>>>()?;
        self.run_query(profile, sql.to_string(), params, max_rows)
            .await
    }

    async fn mutate(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
    ) -> Result<MutationOutcome> {
        if !matches!(
            profile.purpose,
            ProfilePurpose::Mutate | ProfilePurpose::Schema
        ) {
            return Err(Error::InvalidInput(
                "write tool requires a mutate or schema profile".to_string(),
            ));
        }
        let params = params.iter().map(json_to_sql).collect::<Result<Vec<_>>>()?;
        let sql = sql.to_string();
        let result = self
            .connection
            .run(profile, false, move |connection| {
                connection
                    .execute(&sql, params_from_iter(params))
                    .map(|row_count| MutationOutcome {
                        row_count: row_count as u64,
                    })
                    .map_err(write_error)
            })
            .await;
        result.map_err(|error| match error {
            Error::ResourceLimit(_) => Error::ExecutionUnknown,
            other => other,
        })
    }

    async fn explain(&self, profile: &Profile, explain_sql: &str) -> Result<Value> {
        if profile.purpose != ProfilePurpose::Query {
            return Err(Error::InvalidInput(
                "explain requires a query profile".to_string(),
            ));
        }
        let explain_sql = explain_sql.trim().to_string();
        if !explain_sql
            .to_ascii_uppercase()
            .starts_with("EXPLAIN QUERY PLAN ")
            || explain_sql.to_ascii_uppercase().contains("ANALYZE")
        {
            return Err(Error::InvalidInput(
                "SQLite explain requires EXPLAIN QUERY PLAN".to_string(),
            ));
        }
        self.connection
            .run(profile, true, move |connection| {
                let mut statement = connection.prepare(&explain_sql).map_err(query_error)?;
                let names = statement
                    .column_names()
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let mut rows = statement
                    .query(params_from_iter(std::iter::empty::<SqlValue>()))
                    .map_err(query_error)?;
                let mut plan = Vec::new();
                while let Some(row) = rows.next().map_err(query_error)? {
                    plan.push(row_to_json(row, &names));
                }
                Ok(Value::Array(plan))
            })
            .await
    }
}

pub struct SqliteMetadata {
    connection: SqliteConnection,
}

impl SqliteMetadata {
    pub fn new(resolver: Arc<dyn CredentialResolver>) -> Self {
        Self::with_connections(resolver, Arc::new(ProfileConnections::default()))
    }

    pub fn with_connections(
        resolver: Arc<dyn CredentialResolver>,
        connections: Arc<ProfileConnections>,
    ) -> Self {
        Self {
            connection: SqliteConnection::new(resolver, connections),
        }
    }
}

fn schema_is_main(schema: Option<&str>) -> bool {
    schema.is_none_or(|value| value == SQLITE_SCHEMA)
}

#[async_trait]
impl MetadataProvider for SqliteMetadata {
    async fn list_tables(&self, profile: &Profile, schema: Option<&str>) -> Result<Value> {
        if !schema_is_main(schema) {
            return Ok(Value::Array(Vec::new()));
        }
        self.connection
            .run(profile, true, move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT name, type FROM sqlite_master
                          WHERE type IN ('table', 'view')
                            AND name NOT LIKE 'sqlite_%'
                          ORDER BY name",
                    )
                    .map_err(query_error)?;
                let mut rows = statement.query([]).map_err(query_error)?;
                let mut tables = Vec::new();
                while let Some(row) = rows.next().map_err(query_error)? {
                    let name: String = row.get(0).map_err(query_error)?;
                    let kind: String = row.get(1).map_err(query_error)?;
                    tables.push(serde_json::json!({
                        "schema": SQLITE_SCHEMA,
                        "name": name,
                        "kind": if kind == "view" { "VIEW" } else { "BASE TABLE" },
                    }));
                }
                Ok(Value::Array(tables))
            })
            .await
    }

    async fn describe_table(
        &self,
        profile: &Profile,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Value> {
        if !schema_is_main(schema) {
            return Ok(describe_payload(
                table,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ));
        }
        let table_name = table.to_string();
        self.connection
            .run(profile, true, move |connection| {
                let mut columns = Vec::new();
                let mut primary_key = Vec::new();
                {
                    let mut statement = connection
                        .prepare(
                            "SELECT name, type, \"notnull\", dflt_value, pk
                               FROM pragma_table_info(?1) ORDER BY cid",
                        )
                        .map_err(query_error)?;
                    let mut rows = statement
                        .query(params_from_iter([table_name.as_str()]))
                        .map_err(query_error)?;
                    while let Some(row) = rows.next().map_err(query_error)? {
                        let name: String = row.get(0).map_err(query_error)?;
                        let data_type: String = row.get(1).map_err(query_error)?;
                        let not_null: i64 = row.get(2).map_err(query_error)?;
                        let default: Option<String> = row.get(3).map_err(query_error)?;
                        let pk: i64 = row.get(4).map_err(query_error)?;
                        if pk > 0 {
                            primary_key.push((pk, name.clone()));
                        }
                        columns.push(serde_json::json!({
                            "name": name,
                            "type": data_type,
                            // SQLite reports INTEGER PRIMARY KEY as notnull = 0;
                            // keep the PostgreSQL-facing shape: key columns are not nullable.
                            "nullable": not_null == 0 && pk == 0,
                            "default": default,
                        }));
                    }
                }
                primary_key.sort();
                let primary_key = primary_key
                    .into_iter()
                    .map(|(_, name)| name)
                    .collect::<Vec<_>>();

                let mut indexes = Vec::new();
                {
                    let mut statement = connection
                        .prepare("SELECT name FROM pragma_index_list(?1) ORDER BY name")
                        .map_err(query_error)?;
                    let mut rows = statement
                        .query(params_from_iter([table_name.as_str()]))
                        .map_err(query_error)?;
                    let mut names = Vec::new();
                    while let Some(row) = rows.next().map_err(query_error)? {
                        names.push(row.get::<_, String>(0).map_err(query_error)?);
                    }
                    for name in names {
                        let definition: Option<String> = connection
                            .query_row(
                                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                                [&name],
                                |row| row.get(0),
                            )
                            .map_err(query_error)?;
                        indexes.push(serde_json::json!({
                            "name": name,
                            "definition": definition.unwrap_or_else(|| "AUTOINDEX".to_string()),
                        }));
                    }
                }

                let mut foreign_keys = Vec::new();
                {
                    let mut statement = connection
                        .prepare(
                            "SELECT id, \"table\", \"from\", \"to\"
                               FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
                        )
                        .map_err(query_error)?;
                    let mut rows = statement
                        .query(params_from_iter([table_name.as_str()]))
                        .map_err(query_error)?;
                    while let Some(row) = rows.next().map_err(query_error)? {
                        let id: i64 = row.get(0).map_err(query_error)?;
                        let references_table: String = row.get(1).map_err(query_error)?;
                        let column: String = row.get(2).map_err(query_error)?;
                        let references_column: Option<String> = row.get(3).map_err(query_error)?;
                        foreign_keys.push(serde_json::json!({
                            "name": format!("fk_{id}"),
                            "column": column,
                            "references_schema": SQLITE_SCHEMA,
                            "references_table": references_table,
                            "references_column": references_column.unwrap_or_default(),
                        }));
                    }
                }

                Ok(describe_payload(
                    &table_name,
                    columns,
                    primary_key,
                    indexes,
                    foreign_keys,
                ))
            })
            .await
    }

    async fn explain(&self, _profile: &Profile, _sql: &str, _params: &[Value]) -> Result<Value> {
        // Explain runs through the executor so the policy layer sees the statement.
        Err(Error::MetadataUnavailable)
    }
}

fn describe_payload(
    table: &str,
    columns: Vec<Value>,
    primary_key: Vec<String>,
    indexes: Vec<Value>,
    foreign_keys: Vec<Value>,
) -> Value {
    serde_json::json!({
        "schema": SQLITE_SCHEMA,
        "table": table,
        "columns": columns,
        "primary_key": primary_key,
        "indexes": indexes,
        "foreign_keys": foreign_keys,
    })
}
