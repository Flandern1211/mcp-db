use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Number, Value};
use tokio_postgres::types::Type;
use tokio_postgres::{Client, NoTls, Row};

use crate::credentials::CredentialResolver;
use crate::error::{Error, Result};
use crate::state::Profile;

pub const DEFAULT_MAX_ROWS: usize = 500;

#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
    pub row_count: usize,
    pub returned_rows: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct MutationOutcome {
    pub row_count: u64,
}

/// Validated SQL execution boundary. Callers must pass `ValidatedSql` from the policy layer.
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    async fn query(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<QueryOutcome>;
    async fn mutate(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
    ) -> Result<MutationOutcome>;
    async fn explain(&self, profile: &Profile, explain_sql: &str) -> Result<Value>;
}

/// Fails closed until credentials and a driver are configured.
pub struct UnavailableExecutor;

#[async_trait]
impl SqlExecutor for UnavailableExecutor {
    async fn query(
        &self,
        _profile: &Profile,
        _sql: &str,
        _params: &[Value],
        _max_rows: usize,
    ) -> Result<QueryOutcome> {
        Err(Error::MetadataUnavailable)
    }

    async fn mutate(
        &self,
        _profile: &Profile,
        _sql: &str,
        _params: &[Value],
    ) -> Result<MutationOutcome> {
        Err(Error::MetadataUnavailable)
    }

    async fn explain(&self, _profile: &Profile, _explain_sql: &str) -> Result<Value> {
        Err(Error::MetadataUnavailable)
    }
}

/// Minimal PostgreSQL executor. Pooling, timeouts and rate limits belong to T-07.
pub struct PostgresExecutor {
    resolver: Arc<dyn CredentialResolver>,
}

impl PostgresExecutor {
    pub fn new(resolver: Arc<dyn CredentialResolver>) -> Self {
        Self { resolver }
    }

    async fn client(&self, profile: &Profile) -> Result<Client> {
        let dsn = self.resolver.resolve(&profile.credential_ref)?;
        let (client, connection) = tokio_postgres::connect(&dsn, NoTls)
            .await
            .map_err(|_| Error::MetadataUnavailable)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }
}

fn row_to_json(row: &Row) -> Value {
    let mut object = serde_json::Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        let name = column.name().to_string();
        let value = column_value(row, index, column.type_());
        object.insert(name, value);
    }
    Value::Object(object)
}

fn column_value(row: &Row, index: usize, column_type: &Type) -> Value {
    macro_rules! typed {
        ($rust:ty) => {
            row.try_get::<_, Option<$rust>>(index)
                .ok()
                .flatten()
                .map(|value| value.into())
                .unwrap_or(Value::Null)
        };
    }
    match *column_type {
        Type::BOOL => typed!(bool),
        Type::CHAR => typed!(i8),
        Type::INT2 => typed!(i16),
        Type::INT4 => typed!(i32),
        Type::INT8 => typed!(i64),
        Type::OID => typed!(u32),
        Type::FLOAT4 => row
            .try_get::<_, Option<f32>>(index)
            .ok()
            .flatten()
            .map(|value| {
                Number::from_f64(value as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            })
            .unwrap_or(Value::Null),
        Type::FLOAT8 => row
            .try_get::<_, Option<f64>>(index)
            .ok()
            .flatten()
            .map(|value| {
                Number::from_f64(value)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            })
            .unwrap_or(Value::Null),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR => typed!(String),
        Type::BYTEA => row
            .try_get::<_, Option<Vec<u8>>>(index)
            .ok()
            .flatten()
            .map(|bytes| {
                Value::String(format!(
                    "0x{}",
                    bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                ))
            })
            .unwrap_or(Value::Null),
        _ => row
            .try_get::<_, Option<String>>(index)
            .ok()
            .flatten()
            .map(Value::String)
            .unwrap_or(Value::Null),
    }
}

#[async_trait]
impl SqlExecutor for PostgresExecutor {
    async fn query(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<QueryOutcome> {
        let client = self.client(profile).await?;
        let bound = bind_params(params)?;
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = bound
            .iter()
            .map(|value| value.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = client
            .query(sql, &refs)
            .await
            .map_err(|_| Error::MetadataUnavailable)?;
        let columns = if rows.is_empty() {
            Vec::new()
        } else {
            rows[0]
                .columns()
                .iter()
                .map(|column| column.name().to_string())
                .collect()
        };
        let row_count = rows.len();
        let returned_rows = row_count.min(max_rows);
        let rows = rows.iter().take(max_rows).map(row_to_json).collect();
        Ok(QueryOutcome {
            columns,
            rows,
            row_count,
            returned_rows,
            truncated: row_count > max_rows,
        })
    }

    async fn mutate(
        &self,
        profile: &Profile,
        sql: &str,
        params: &[Value],
    ) -> Result<MutationOutcome> {
        let client = self.client(profile).await?;
        let bound = bind_params(params)?;
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = bound
            .iter()
            .map(|value| value.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let row_count = client
            .execute(sql, &refs)
            .await
            .map_err(|_| Error::MetadataUnavailable)?;
        Ok(MutationOutcome { row_count })
    }

    async fn explain(&self, profile: &Profile, explain_sql: &str) -> Result<Value> {
        let client = self.client(profile).await?;
        let rows = client
            .query(explain_sql, &[])
            .await
            .map_err(|_| Error::MetadataUnavailable)?;
        let Some(row) = rows.first() else {
            return Ok(Value::Null);
        };
        let plan: String = row.try_get(0).map_err(|_| Error::MetadataUnavailable)?;
        serde_json::from_str(&plan).map_err(|_| Error::MetadataUnavailable)
    }
}

fn bind_params(
    params: &[Value],
) -> Result<Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>>> {
    params
        .iter()
        .map(|value| {
            Ok(match value {
                Value::Null => Box::new(Option::<String>::None) as _,
                Value::Bool(b) => Box::new(*b) as _,
                Value::Number(number) => {
                    if let Some(i) = number.as_i64() {
                        Box::new(i) as _
                    } else {
                        Box::new(number.as_f64().unwrap_or_default()) as _
                    }
                }
                Value::String(s) => Box::new(s.clone()) as _,
                other => {
                    return Err(Error::SqlRejected(format!(
                        "parameter type {} is not supported",
                        type_name(other)
                    )))
                }
            })
        })
        .collect()
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
