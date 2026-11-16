use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::auth;
use crate::error::{Error, Result};
use crate::state::model::{
    validate_credential_ref, validate_limit, validate_profile_name, validate_source_id,
    ApprovalPolicy, Driver, Environment, Grant, IssuedToken, NewGrant, NewProfile, NewRoute,
    NewSource, Profile, ProfilePurpose, Route, Source, SourceStatus, TokenIdentity, Tool,
};

const SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
  id TEXT PRIMARY KEY,
  driver TEXT NOT NULL CHECK (driver IN ('postgres')),
  environment TEXT NOT NULL CHECK (environment IN ('production', 'nonproduction')),
  status TEXT NOT NULL CHECK (status IN ('active', 'archived')),
  parent_source_id TEXT REFERENCES sources(id),
  created_token_id TEXT REFERENCES tokens(id),
  created_agent TEXT,
  created_origin TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  archived_at INTEGER
);
CREATE INDEX IF NOT EXISTS sources_status ON sources(status, environment);

CREATE TABLE IF NOT EXISTS connection_profiles (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL REFERENCES sources(id),
  name TEXT NOT NULL,
  purpose TEXT NOT NULL CHECK (purpose IN ('query', 'mutate', 'schema', 'provision')),
  credential_ref TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  max_connections INTEGER CHECK (max_connections IS NULL OR max_connections > 0),
  statement_timeout_ms INTEGER CHECK (statement_timeout_ms IS NULL OR statement_timeout_ms > 0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (source_id, name)
);
CREATE INDEX IF NOT EXISTS connection_profiles_source ON connection_profiles(source_id, purpose);

CREATE TABLE IF NOT EXISTS source_tool_routes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL REFERENCES sources(id),
  tool TEXT NOT NULL CHECK (tool IN (
    'list_tables', 'describe_table', 'explain_query', 'query_sql',
    'mutate_sql', 'execute_ddl', 'create_database'
  )),
  profile_id INTEGER NOT NULL REFERENCES connection_profiles(id),
  min_approval_policy TEXT NOT NULL CHECK (min_approval_policy IN ('direct', 'approval')),
  version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (source_id, tool)
);
CREATE INDEX IF NOT EXISTS source_tool_routes_profile ON source_tool_routes(profile_id);

CREATE TABLE IF NOT EXISTS tokens (
  id TEXT PRIMARY KEY,
  agent TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK (status IN ('active', 'revoked')),
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS tokens_status ON tokens(status);

CREATE TABLE IF NOT EXISTS tool_grants (
  token_id TEXT NOT NULL REFERENCES tokens(id),
  source_id TEXT NOT NULL REFERENCES sources(id),
  tool TEXT NOT NULL CHECK (tool IN (
    'list_tables', 'describe_table', 'explain_query', 'query_sql',
    'mutate_sql', 'execute_ddl', 'create_database'
  )),
  policy TEXT NOT NULL CHECK (policy IN ('direct', 'approval')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (token_id, source_id, tool)
);
CREATE INDEX IF NOT EXISTS tool_grants_source ON tool_grants(source_id, tool);

-- Legacy v1 tables stay readable. New audit/approval writes are owned by T-06.
CREATE TABLE IF NOT EXISTS approvals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  agent TEXT NOT NULL,
  source TEXT NOT NULL,
  tool TEXT NOT NULL,
  sql TEXT NOT NULL,
  params_json TEXT,
  status TEXT NOT NULL,
  channel TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  decided_at INTEGER
);
CREATE INDEX IF NOT EXISTS approvals_status ON approvals(status, created_at);
CREATE TABLE IF NOT EXISTS audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  agent TEXT NOT NULL,
  source TEXT,
  tool TEXT NOT NULL,
  sql TEXT,
  params_json TEXT,
  duration_ms INTEGER,
  row_count INTEGER,
  decision TEXT NOT NULL,
  channel TEXT,
  reason TEXT
);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| {
                    Error::InvalidInput(format!("cannot create state directory: {error}"))
                })?;
            }
        }
        let mut conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 3000;",
        )?;

        let transaction = conn.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
               version INTEGER PRIMARY KEY,
               applied_at INTEGER NOT NULL
             );",
        )?;
        let version: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version > SCHEMA_VERSION {
            return Err(Error::InvalidInput(format!(
                "state schema version {version} is newer than supported {SCHEMA_VERSION}"
            )));
        }
        if version < 1 {
            transaction.execute_batch(MIGRATION_1)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?)",
                [now_ms()],
            )?;
        }
        transaction.commit()?;

        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn create_token(&self, agent: &str) -> Result<IssuedToken> {
        let agent = agent.trim();
        if agent.is_empty() || agent.len() > 128 {
            return Err(Error::InvalidInput(
                "agent must be 1-128 characters".to_string(),
            ));
        }
        let token = auth::generate_token();
        let token_id = auth::generate_token_id();
        let token_hash = auth::hash_token(&token);
        self.conn.execute(
            "INSERT INTO tokens (id, agent, token_hash, status, created_at) VALUES (?, ?, ?, 'active', ?)",
            params![token_id, agent, token_hash, now_ms()],
        )?;
        Ok(IssuedToken {
            token,
            token_id,
            agent: agent.to_string(),
        })
    }

    pub fn authenticate(&self, raw_token: &str) -> Result<TokenIdentity> {
        let hash = auth::hash_token(raw_token);
        let row = self
            .conn
            .query_row(
                "SELECT id, agent, status FROM tokens WHERE token_hash = ?",
                [hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((token_id, agent, status)) = row else {
            return Err(Error::Unauthenticated);
        };
        if status == "revoked" {
            return Err(Error::TokenRevoked);
        }
        Ok(TokenIdentity { token_id, agent })
    }

    pub fn revoke_token(&self, token_id: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tokens SET status = 'revoked', revoked_at = ? WHERE id = ? AND status = 'active'",
            params![now_ms(), token_id],
        )?;
        if changed > 0 {
            return Ok(());
        }
        let exists: bool = self
            .conn
            .query_row("SELECT 1 FROM tokens WHERE id = ?", [token_id], |_| {
                Ok(true)
            })
            .optional()?
            .unwrap_or(false);
        if exists {
            Ok(())
        } else {
            Err(Error::NotFound(format!("token {token_id}")))
        }
    }

    pub fn create_source(&self, source: &NewSource) -> Result<Source> {
        validate_source_id(&source.id)?;
        if let Some(parent_id) = &source.parent_source_id {
            if parent_id == &source.id {
                return Err(Error::InvalidInput(
                    "source cannot be its own parent".to_string(),
                ));
            }
            let parent = self.get_source(parent_id)?;
            if parent.status != SourceStatus::Active {
                return Err(Error::InvalidInput(
                    "parent source must be active".to_string(),
                ));
            }
        }
        if let Some(token_id) = &source.created_token_id {
            self.active_token(token_id)?;
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO sources
               (id, driver, environment, status, parent_source_id, created_token_id,
                created_agent, created_origin, created_at, updated_at)
             VALUES (?, ?, ?, 'active', ?, ?, ?, ?, ?, ?)",
            params![
                source.id,
                source.driver.as_str(),
                source.environment.as_str(),
                source.parent_source_id,
                source.created_token_id,
                source.created_agent,
                source.created_origin,
                now,
                now
            ],
        )?;
        self.get_source(&source.id)
    }

    pub fn get_source(&self, id: &str) -> Result<Source> {
        let row = self
            .conn
            .query_row(
                "SELECT id, driver, environment, status, parent_source_id, created_at, updated_at, archived_at
                   FROM sources WHERE id = ?",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            driver,
            environment,
            status,
            parent_source_id,
            created_at,
            updated_at,
            archived_at,
        )) = row
        else {
            return Err(Error::NotFound(format!("source {id}")));
        };
        Ok(Source {
            id,
            driver: Driver::parse(&driver)?,
            environment: Environment::parse(&environment)?,
            status: SourceStatus::parse(&status)?,
            parent_source_id,
            created_at,
            updated_at,
            archived_at,
        })
    }

    pub fn create_profile(&self, profile: &NewProfile) -> Result<Profile> {
        let source = self.get_source(&profile.source_id)?;
        if source.status != SourceStatus::Active {
            return Err(Error::InvalidInput(
                "cannot add profile to an archived source".to_string(),
            ));
        }
        validate_profile_name(&profile.name)?;
        validate_credential_ref(&profile.credential_ref)?;
        validate_limit(profile.max_connections.map(u64::from), "max_connections")?;
        validate_limit(profile.statement_timeout_ms, "statement_timeout_ms")?;
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO connection_profiles
               (source_id, name, purpose, credential_ref, enabled, version,
                max_connections, statement_timeout_ms, created_at, updated_at)
             VALUES (?, ?, ?, ?, 1, 1, ?, ?, ?, ?)",
            params![
                profile.source_id,
                profile.name,
                profile.purpose.as_str(),
                profile.credential_ref,
                profile.max_connections.map(i64::from),
                profile.statement_timeout_ms.map(|value| value as i64),
                now,
                now
            ],
        )?;
        self.get_profile(self.conn.last_insert_rowid())
    }

    pub fn get_profile(&self, id: i64) -> Result<Profile> {
        let row = self
            .conn
            .query_row(
                "SELECT id, source_id, name, purpose, credential_ref, enabled, version,
                        max_connections, statement_timeout_ms
                   FROM connection_profiles WHERE id = ?",
                [id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            source_id,
            name,
            purpose,
            credential_ref,
            enabled,
            version,
            max_connections,
            statement_timeout_ms,
        )) = row
        else {
            return Err(Error::NotFound(format!("profile {id}")));
        };
        Ok(Profile {
            id,
            source_id,
            name,
            purpose: ProfilePurpose::parse(&purpose)?,
            credential_ref,
            enabled: enabled != 0,
            version,
            max_connections: max_connections.map(|value| value as u32),
            statement_timeout_ms: statement_timeout_ms.map(|value| value as u64),
        })
    }

    pub fn list_profiles(&self, source_id: &str) -> Result<Vec<Profile>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM connection_profiles WHERE source_id = ? ORDER BY name")?;
        let ids = statement
            .query_map([source_id], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| self.get_profile(id)).collect()
    }

    pub fn create_route(&self, route: &NewRoute) -> Result<Route> {
        let source = self.get_source(&route.source_id)?;
        if source.status != SourceStatus::Active {
            return Err(Error::InvalidInput(
                "cannot add route to an archived source".to_string(),
            ));
        }
        let profile = self.get_profile(route.profile_id)?;
        if profile.source_id != route.source_id {
            return Err(Error::InvalidInput(
                "route profile belongs to a different source".to_string(),
            ));
        }
        let Some(purpose) = route.tool.purpose() else {
            return Err(Error::InvalidInput(
                "list_sources is a discovery tool and cannot have a route".to_string(),
            ));
        };
        if purpose != profile.purpose {
            return Err(Error::InvalidInput(format!(
                "tool {} requires a {} profile",
                route.tool.as_str(),
                purpose.as_str()
            )));
        }
        validate_route_policy(route.tool, route.min_approval_policy)?;
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO source_tool_routes
               (source_id, tool, profile_id, min_approval_policy, version, created_at, updated_at)
             VALUES (?, ?, ?, ?, 1, ?, ?)",
            params![
                route.source_id,
                route.tool.as_str(),
                route.profile_id,
                route.min_approval_policy.as_str(),
                now,
                now
            ],
        )?;
        self.get_route(&route.source_id, route.tool)
    }

    pub fn get_route(&self, source_id: &str, tool: Tool) -> Result<Route> {
        let row = self
            .conn
            .query_row(
                "SELECT id, source_id, tool, profile_id, min_approval_policy, version
                   FROM source_tool_routes WHERE source_id = ? AND tool = ?",
                params![source_id, tool.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, source_id, tool_text, profile_id, policy, version)) = row else {
            return Err(Error::NotFound(format!(
                "route {source_id}/{}",
                tool.as_str()
            )));
        };
        Ok(Route {
            id,
            source_id,
            tool: Tool::parse(&tool_text)?,
            profile_id,
            min_approval_policy: ApprovalPolicy::parse(&policy)?,
            version,
        })
    }

    pub fn create_grant(&self, grant: &NewGrant) -> Result<Grant> {
        self.active_token(&grant.token_id)?;
        let source = self.get_source(&grant.source_id)?;
        if source.status != SourceStatus::Active {
            return Err(Error::InvalidInput(
                "cannot grant access to an archived source".to_string(),
            ));
        }
        if grant.tool == Tool::ListSources {
            return Err(Error::InvalidInput(
                "list_sources is available to every authenticated token".to_string(),
            ));
        }
        if source.environment == Environment::Production && grant.tool.is_write() {
            return Err(Error::InvalidInput(
                "production sources cannot grant write tools".to_string(),
            ));
        }
        let policy = grant.policy.unwrap_or_else(|| default_policy(grant.tool));
        validate_grant_policy(grant.tool, policy)?;
        self.conn.execute(
            "INSERT INTO tool_grants (token_id, source_id, tool, policy, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![
                grant.token_id,
                grant.source_id,
                grant.tool.as_str(),
                policy.as_str(),
                now_ms()
            ],
        )?;
        Ok(Grant {
            token_id: grant.token_id.clone(),
            source_id: grant.source_id.clone(),
            tool: grant.tool,
            policy,
        })
    }

    pub fn get_grant(&self, token_id: &str, source_id: &str, tool: Tool) -> Result<Grant> {
        let row = self
            .conn
            .query_row(
                "SELECT token_id, source_id, tool, policy
                   FROM tool_grants WHERE token_id = ? AND source_id = ? AND tool = ?",
                params![token_id, source_id, tool.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((token_id, source_id, tool_text, policy)) = row else {
            return Err(Error::NotFound(format!(
                "grant {source_id}/{}",
                tool.as_str()
            )));
        };
        Ok(Grant {
            token_id,
            source_id,
            tool: Tool::parse(&tool_text)?,
            policy: ApprovalPolicy::parse(&policy)?,
        })
    }

    pub fn list_grants_for_token(&self, token_id: &str) -> Result<Vec<Grant>> {
        let mut statement = self.conn.prepare(
            "SELECT source_id, tool, policy FROM tool_grants
              WHERE token_id = ? ORDER BY source_id, tool",
        )?;
        let rows = statement
            .query_map([token_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(source_id, tool, policy)| {
                Ok(Grant {
                    token_id: token_id.to_string(),
                    source_id,
                    tool: Tool::parse(&tool)?,
                    policy: ApprovalPolicy::parse(&policy)?,
                })
            })
            .collect()
    }

    fn active_token(&self, token_id: &str) -> Result<TokenIdentity> {
        let row = self
            .conn
            .query_row(
                "SELECT agent, status FROM tokens WHERE id = ?",
                [token_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((agent, status)) = row else {
            return Err(Error::NotFound(format!("token {token_id}")));
        };
        if status == "revoked" {
            return Err(Error::TokenRevoked);
        }
        Ok(TokenIdentity {
            token_id: token_id.to_string(),
            agent,
        })
    }
}

fn validate_route_policy(tool: Tool, policy: ApprovalPolicy) -> Result<()> {
    if tool == Tool::CreateDatabase {
        return if policy == ApprovalPolicy::Approval {
            Ok(())
        } else {
            Err(Error::InvalidInput(
                "create_database always requires approval".to_string(),
            ))
        };
    }
    if tool.is_write() {
        return Ok(());
    }
    if policy == ApprovalPolicy::Direct {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "read-only tools cannot require approval".to_string(),
        ))
    }
}

fn validate_grant_policy(tool: Tool, policy: ApprovalPolicy) -> Result<()> {
    if tool == Tool::CreateDatabase {
        return if policy == ApprovalPolicy::Approval {
            Ok(())
        } else {
            Err(Error::InvalidInput(
                "create_database grants always require approval".to_string(),
            ))
        };
    }
    if tool.is_write() {
        return Ok(());
    }
    if policy == ApprovalPolicy::Direct {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "read-only grants cannot use approval policy".to_string(),
        ))
    }
}

fn default_policy(tool: Tool) -> ApprovalPolicy {
    if tool.is_write() || tool == Tool::CreateDatabase {
        ApprovalPolicy::Approval
    } else {
        ApprovalPolicy::Direct
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
