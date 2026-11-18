use sqlparser::ast::{ObjectType, Query, SetExpr, Statement};
use sqlparser::dialect::{PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::error::{Error, Result};
use crate::state::{Driver, Tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlClass {
    Query,
    Mutate,
    Ddl,
}

impl SqlClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutate => "mutate",
            Self::Ddl => "ddl",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSql {
    pub sql: String,
    pub class: SqlClass,
    pub driver: Driver,
    pub high_risk: bool,
}

impl ValidatedSql {
    pub fn explain_sql(&self) -> String {
        match self.driver {
            Driver::Postgres => format!("EXPLAIN (FORMAT JSON) {}", self.sql),
            Driver::Sqlite => format!("EXPLAIN QUERY PLAN {}", self.sql),
        }
    }
}

/// Parse one PostgreSQL statement and apply the first-release tool whitelist.
pub fn validate(sql: &str) -> Result<ValidatedSql> {
    validate_for_driver(sql, Driver::Postgres)
}

/// Parse one statement in the source driver's dialect. Unknown or driver-specific
/// administrative statements fail closed instead of falling back to generic SQL.
pub fn validate_for_driver(sql: &str, driver: Driver) -> Result<ValidatedSql> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(Error::SqlRejected("empty SQL".to_string()));
    }
    if driver == Driver::Sqlite && contains_load_extension(sql) {
        return Err(unsupported("load_extension"));
    }
    let statements = match driver {
        Driver::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        Driver::Sqlite => Parser::parse_sql(&SQLiteDialect {}, sql),
    }
    .map_err(|error| Error::SqlRejected(format!("parse failed: {error}")))?;
    if statements.len() != 1 {
        return Err(Error::SqlRejected(
            "exactly one statement is required".to_string(),
        ));
    }
    let statement = statements
        .into_iter()
        .next()
        .ok_or_else(|| Error::SqlRejected("empty SQL".to_string()))?;

    let (class, high_risk) = classify(statement, driver)?;

    Ok(ValidatedSql {
        sql: sql.trim_end_matches(';').trim_end().to_string(),
        class,
        driver,
        high_risk,
    })
}

fn classify(statement: Statement, driver: Driver) -> Result<(SqlClass, bool)> {
    if driver == Driver::Sqlite {
        // These statements are either SQLite-only administration or not SQLite at
        // all. Reject them before the shared classification can accept a near miss.
        match &statement {
            Statement::AttachDatabase { .. }
            | Statement::AttachDuckDBDatabase { .. }
            | Statement::DetachDuckDBDatabase { .. } => return Err(unsupported("ATTACH/DETACH")),
            Statement::Pragma { .. } => return Err(unsupported("PRAGMA")),
            Statement::Vacuum(_) => return Err(unsupported("VACUUM")),
            Statement::Analyze(_) => return Err(unsupported("ANALYZE")),
            Statement::Truncate(_) => return Err(unsupported("TRUNCATE (not a SQLite statement)")),
            Statement::CreateTrigger(_) | Statement::CreateVirtualTable { .. } => {
                return Err(unsupported("trigger or virtual table"))
            }
            _ => {}
        }
    }
    shared_classification(statement)
}

fn shared_classification(statement: Statement) -> Result<(SqlClass, bool)> {
    let classified = match statement {
        Statement::Query(query) => {
            validate_read_query(&query)?;
            (SqlClass::Query, false)
        }
        Statement::Insert(_) => (SqlClass::Mutate, false),
        Statement::Update(update) => (SqlClass::Mutate, update.selection.is_none()),
        Statement::Delete(delete) => (SqlClass::Mutate, delete.selection.is_none()),
        Statement::Truncate(_) => (SqlClass::Mutate, true),
        Statement::CreateTable(_) | Statement::CreateIndex(_) | Statement::CreateView(_) => {
            (SqlClass::Ddl, false)
        }
        Statement::AlterTable(_) => (SqlClass::Ddl, true),
        Statement::Drop { object_type, .. } => match object_type {
            ObjectType::Table | ObjectType::View | ObjectType::Index => (SqlClass::Ddl, true),
            _ => return Err(unsupported("database, schema, role or other global object")),
        },
        other => {
            return Err(unsupported(&format!(
                "statement type {}",
                statement_kind(&other)
            )))
        }
    };
    Ok(classified)
}

fn contains_load_extension(sql: &str) -> bool {
    sql.to_ascii_lowercase()
        .replace([' ', '\t', '\n', '\r'], "")
        .contains("load_extension")
}

pub fn validate_for_tool(sql: &str, tool: Tool) -> Result<ValidatedSql> {
    validate_for_driver_tool(sql, tool, Driver::Postgres)
}

pub fn validate_for_driver_tool(sql: &str, tool: Tool, driver: Driver) -> Result<ValidatedSql> {
    let validated = validate_for_driver(sql, driver)?;
    let expected = match tool {
        Tool::QuerySql | Tool::ExplainQuery => SqlClass::Query,
        Tool::MutateSql => SqlClass::Mutate,
        Tool::ExecuteDdl => SqlClass::Ddl,
        _ => {
            return Err(Error::SqlRejected(format!(
                "tool {} does not accept SQL",
                tool.as_str()
            )))
        }
    };
    if validated.class != expected {
        return Err(Error::SqlRejected(format!(
            "tool {} requires a {} statement",
            tool.as_str(),
            expected.as_str()
        )));
    }
    Ok(validated)
}

fn validate_read_query(query: &Query) -> Result<()> {
    if !query.locks.is_empty() {
        return Err(Error::SqlRejected(
            "locking SELECT is not allowed".to_string(),
        ));
    }
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            validate_read_query(&cte.query)?;
        }
    }
    validate_set_expr(&query.body)
}

fn validate_set_expr(expression: &SetExpr) -> Result<()> {
    match expression {
        SetExpr::Select(select) => {
            if select.into.is_some() {
                return Err(Error::SqlRejected(
                    "SELECT INTO creates a table".to_string(),
                ));
            }
            Ok(())
        }
        SetExpr::Query(query) => validate_read_query(query),
        SetExpr::SetOperation { left, right, .. } => {
            validate_set_expr(left)?;
            validate_set_expr(right)
        }
        SetExpr::Values(_) => Ok(()),
        _ => Err(Error::SqlRejected(
            "only read-only SELECT, CTE, VALUES or set operations are allowed".to_string(),
        )),
    }
}

fn unsupported(kind: &str) -> Error {
    Error::SqlRejected(format!("{kind} is not supported"))
}

fn statement_kind(statement: &Statement) -> &'static str {
    match statement {
        Statement::Analyze(_) => "ANALYZE",
        Statement::Set(_) => "SET",
        Statement::Call(_) => "CALL",
        Statement::Copy { .. } => "COPY",
        Statement::Merge(_) => "MERGE",
        Statement::Explain { .. } => "EXPLAIN",
        Statement::CreateSchema { .. } => "CREATE SCHEMA",
        Statement::CreateDatabase { .. } => "CREATE DATABASE",
        Statement::CreateRole(_) => "CREATE ROLE",
        Statement::CreateUser(_) => "CREATE USER",
        Statement::AlterRole { .. } => "ALTER ROLE",
        Statement::Grant(_) | Statement::GrantRole(_) => "GRANT",
        Statement::Revoke(_) | Statement::RevokeRole(_) => "REVOKE",
        Statement::Drop { .. } => "DROP",
        Statement::Use(_) => "USE",
        Statement::StartTransaction { .. } => "START TRANSACTION",
        Statement::Commit { .. } => "COMMIT",
        Statement::Rollback { .. } => "ROLLBACK",
        _ => "this statement",
    }
}
