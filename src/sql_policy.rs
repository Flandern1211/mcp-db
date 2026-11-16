use sqlparser::ast::{ObjectType, Query, SetExpr, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::error::{Error, Result};
use crate::state::Tool;

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
    pub high_risk: bool,
}

impl ValidatedSql {
    pub fn explain_sql(&self) -> String {
        format!("EXPLAIN (FORMAT JSON) {}", self.sql)
    }
}

/// Parse one PostgreSQL statement and apply the first-release tool whitelist.
pub fn validate(sql: &str) -> Result<ValidatedSql> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(Error::SqlRejected("empty SQL".to_string()));
    }
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql)
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

    let (class, high_risk) = match statement {
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

    Ok(ValidatedSql {
        sql: sql.trim_end_matches(';').trim_end().to_string(),
        class,
        high_risk,
    })
}

pub fn validate_for_tool(sql: &str, tool: Tool) -> Result<ValidatedSql> {
    let validated = validate(sql)?;
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
