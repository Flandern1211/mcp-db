use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::approval::{
    ApprovalChannel, ApprovalDraft, ApprovalOutcome, ApprovalRecord, ApprovalStatus, AuditEvent,
    ClaimOutcome, DecisionOutcome, NewApprovalRequest, RecoverySummary,
};
use crate::auth;
use crate::error::{Error, Result};
use crate::state::model::{
    validate_credential_ref, validate_profile_name, validate_source_id, AdminKey, ApprovalPolicy,
    Driver, Environment, Grant, IssuedToken, NewGrant, NewProfile, NewRoute, NewSource, Profile,
    ProfilePurpose, ProfileUpdate, ProvisionUsage, QuotaReservation, ReservationStatus, Route,
    RouteUpdate, Source, SourceStatus, TokenIdentity, TokenRecord, Tool,
};

const SCHEMA_VERSION: i64 = 6;

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

const MIGRATION_2: &str = r#"
CREATE TABLE IF NOT EXISTS approval_requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  token_id TEXT NOT NULL,
  agent TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_environment TEXT NOT NULL,
  tool TEXT NOT NULL CHECK (tool IN ('mutate_sql', 'execute_ddl', 'create_database')),
  sql TEXT,
  params_json TEXT NOT NULL,
  request_digest TEXT NOT NULL,
  profile_id INTEGER NOT NULL,
  profile_version INTEGER NOT NULL,
  route_id INTEGER NOT NULL,
  route_version INTEGER NOT NULL,
  grant_policy TEXT NOT NULL CHECK (grant_policy IN ('direct', 'approval')),
  route_min_policy TEXT NOT NULL CHECK (route_min_policy IN ('direct', 'approval')),
  high_risk INTEGER NOT NULL CHECK (high_risk IN (0, 1)),
  channel TEXT NOT NULL CHECK (channel IN ('elicitation', 'admin')),
  status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'denied', 'expired', 'consumed')),
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  decided_at INTEGER,
  decided_by TEXT,
  consumed_at INTEGER,
  outcome TEXT CHECK (outcome IS NULL OR outcome IN ('succeeded', 'failed', 'unknown')),
  finished_at INTEGER,
  row_count INTEGER,
  error_code TEXT,
  reason_code TEXT,
  reason TEXT
);
CREATE INDEX IF NOT EXISTS approval_requests_queue
  ON approval_requests(status, created_at);
CREATE INDEX IF NOT EXISTS approval_requests_expiry
  ON approval_requests(status, expires_at);
CREATE INDEX IF NOT EXISTS approval_requests_token
  ON approval_requests(token_id, status);
CREATE INDEX IF NOT EXISTS approval_requests_source
  ON approval_requests(source_id, status);

CREATE TABLE IF NOT EXISTS audit_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  event TEXT NOT NULL,
  approval_id INTEGER,
  token_id TEXT,
  agent TEXT,
  source_id TEXT,
  tool TEXT NOT NULL,
  profile_id INTEGER,
  profile_version INTEGER,
  route_id INTEGER,
  route_version INTEGER,
  request_digest TEXT,
  channel TEXT,
  decision TEXT NOT NULL,
  actor TEXT NOT NULL,
  high_risk INTEGER NOT NULL CHECK (high_risk IN (0, 1)),
  reason_code TEXT,
  reason TEXT,
  duration_ms INTEGER,
  row_count INTEGER,
  error_code TEXT
);
CREATE INDEX IF NOT EXISTS audit_events_approval ON audit_events(approval_id, id);
CREATE INDEX IF NOT EXISTS audit_events_ts ON audit_events(ts);
"#;

const MIGRATION_3: &str = r#"
CREATE TABLE IF NOT EXISTS quota_reservations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  token_id TEXT NOT NULL REFERENCES tokens(id),
  parent_source_id TEXT NOT NULL REFERENCES sources(id),
  database_name TEXT NOT NULL UNIQUE,
  approval_id INTEGER REFERENCES approval_requests(id),
  status TEXT NOT NULL CHECK (status IN ('reserved', 'committed', 'released', 'quarantined')),
  reserved_at INTEGER NOT NULL,
  settled_at INTEGER,
  reason_code TEXT
);
CREATE INDEX IF NOT EXISTS quota_reservations_scope
  ON quota_reservations(token_id, parent_source_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS quota_reservations_approval
  ON quota_reservations(approval_id) WHERE approval_id IS NOT NULL;
"#;

/// One-time admin login tokens. Only the SHA-256 hash is stored; the plaintext is
/// printed once by `mcp-db admin-login-token`.
const MIGRATION_4: &str = r#"
CREATE TABLE IF NOT EXISTS admin_login_tokens (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  token_hash TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER
);
CREATE INDEX IF NOT EXISTS admin_login_tokens_expiry
  ON admin_login_tokens(expires_at);
"#;

/// SQLite cannot change a CHECK constraint in place. `Store::open_admin` runs this
/// with foreign keys disabled, then revalidates every reference before committing.
const MIGRATION_5: &str = r#"
CREATE TABLE sources_v5 (
  id TEXT PRIMARY KEY,
  driver TEXT NOT NULL CHECK (driver IN ('postgres', 'sqlite')),
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
INSERT INTO sources_v5
  (id, driver, environment, status, parent_source_id, created_token_id,
   created_agent, created_origin, created_at, updated_at, archived_at)
SELECT
  id, driver, environment, status, parent_source_id, created_token_id,
  created_agent, created_origin, created_at, updated_at, archived_at
FROM sources;
DROP TABLE sources;
ALTER TABLE sources_v5 RENAME TO sources;
CREATE INDEX sources_status ON sources(status, environment);
"#;

/// Long-lived admin keys. They never expire; only a hash is stored and a key
/// can be revoked at any time. `last_used_at` is for operator visibility.
const MIGRATION_6: &str = r#"
CREATE TABLE IF NOT EXISTS admin_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  key_hash TEXT NOT NULL UNIQUE,
  label TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER,
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS admin_keys_active
  ON admin_keys(revoked_at);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self::open_admin(path)?;
        store.recover_after_restart(now_ms())?;
        Ok(store)
    }

    /// Admin access to a running service must not run service-start recovery.
    pub fn open_admin(path: impl AsRef<Path>) -> Result<Self> {
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
        let mut version: i64 = transaction.query_row(
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
            version = 1;
        }
        if version < 2 {
            transaction.execute_batch(MIGRATION_2)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?)",
                [now_ms()],
            )?;
            version = 2;
        }
        if version < 3 {
            transaction.execute_batch(MIGRATION_3)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (3, ?)",
                [now_ms()],
            )?;
            version = 3;
        }
        if version < 4 {
            transaction.execute_batch(MIGRATION_4)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (4, ?)",
                [now_ms()],
            )?;
            version = 4;
        }
        transaction.commit()?;

        let sources_sql: String = conn.query_row(
            "SELECT COALESCE(
               (SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'sources'),
               '')",
            [],
            |row| row.get(0),
        )?;
        // A v5 state created by the T-13 branch has admin_keys but not the SQLite
        // driver in the sources CHECK. Repair it instead of trusting the number.
        let needs_source_rebuild = !sources_sql.contains("'sqlite'");

        if version < 5 || needs_source_rebuild {
            // The sources CHECK cannot be altered in place. Foreign keys must be
            // disabled outside a transaction; the rebuilt table is validated with
            // `pragma_foreign_key_check` before the v5 transaction commits.
            conn.execute_batch("PRAGMA foreign_keys = OFF")?;
            let transaction = conn.transaction()?;
            transaction.execute_batch(MIGRATION_5)?;
            let violations: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )?;
            if violations > 0 {
                return Err(Error::InvalidInput(
                    "state migration would break foreign keys".to_string(),
                ));
            }
            if version < 5 {
                transaction.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (5, ?)",
                    [now_ms()],
                )?;
                version = 5;
            }
            transaction.commit()?;
            conn.execute_batch("PRAGMA foreign_keys = ON")?;
        }

        if version < 6 {
            let transaction = conn.transaction()?;
            transaction.execute_batch(MIGRATION_6)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (6, ?)",
                [now_ms()],
            )?;
            transaction.commit()?;
        }

        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    /// Flushes the WAL into the main database file before an atomic file swap.
    pub fn checkpoint(&self) -> Result<()> {
        let _: (i64, i64, i64) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        Ok(())
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

    /// Imports a pre-existing agent token by hash. Only the hash is persisted; the
    /// caller keeps the plaintext in memory and never writes it to disk.
    pub fn import_token(&self, agent: &str, token_hash: &str) -> Result<String> {
        let agent = agent.trim();
        if agent.is_empty() || agent.len() > 128 {
            return Err(Error::InvalidInput(
                "agent must be 1-128 characters".to_string(),
            ));
        }
        let token_hash = token_hash.trim().to_ascii_lowercase();
        if token_hash.len() != 64 || !token_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::InvalidInput(
                "token hash must be a 64-character SHA-256 hex value".to_string(),
            ));
        }
        let token_id = auth::generate_token_id();
        self.conn.execute(
            "INSERT INTO tokens (id, agent, token_hash, status, created_at) VALUES (?, ?, ?, 'active', ?)",
            params![token_id, agent, token_hash, now_ms()],
        )?;
        Ok(token_id)
    }

    /// Administrative token listing. Only identifying metadata is returned; the stored
    /// hash and the one-time plaintext are never exposed.
    pub fn list_tokens(&self) -> Result<Vec<TokenRecord>> {
        let mut statement = self.conn.prepare(
            "SELECT id, agent, status, created_at, revoked_at
               FROM tokens ORDER BY created_at DESC, id DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(TokenRecord {
                    id: row.get(0)?,
                    agent: row.get(1)?,
                    status: row.get(2)?,
                    created_at: row.get(3)?,
                    revoked_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
        let now = now_ms();
        let changed = self.conn.execute(
            "UPDATE tokens SET status = 'revoked', revoked_at = ? WHERE id = ? AND status = 'active'",
            params![now, token_id],
        )?;
        if changed > 0 {
            self.invalidate_approvals_for_token(token_id, "TOKEN_REVOKED", now)?;
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

    /// Issues a new one-time admin login token and returns its plaintext exactly once.
    pub fn create_admin_login_token(&self, now: i64, ttl_ms: i64) -> Result<String> {
        // Expired one-time tokens have no further use; drop them so the table cannot
        // grow without bound.
        self.conn.execute(
            "DELETE FROM admin_login_tokens WHERE expires_at <= ?",
            [now],
        )?;
        let token = auth::generate_admin_login_token();
        let hash = auth::hash_token(&token);
        self.conn.execute(
            "INSERT INTO admin_login_tokens (token_hash, created_at, expires_at) VALUES (?, ?, ?)",
            params![hash, now, now.saturating_add(ttl_ms)],
        )?;
        Ok(token)
    }

    /// Consumes a login token exactly once. Any unknown, used or expired token is
    /// rejected with the same error so callers cannot distinguish the cases.
    pub fn consume_admin_login_token(&self, raw_token: &str, now: i64) -> Result<()> {
        let hash = auth::hash_token(raw_token.trim());
        let changed = self.conn.execute(
            "UPDATE admin_login_tokens SET used_at = ?
              WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?",
            params![now, hash, now],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::Unauthenticated)
        }
    }

    /// Creates a long-lived admin key and returns its plaintext exactly once.
    /// The key never expires; only a SHA-256 hash is persisted.
    pub fn create_admin_key(&self, label: &str, now: i64) -> Result<String> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 || label.chars().any(|c| c.is_control()) {
            return Err(Error::InvalidInput(
                "admin key label must be 1-64 printable characters".to_string(),
            ));
        }
        let key = auth::generate_admin_key();
        let hash = auth::hash_token(&key);
        self.conn.execute(
            "INSERT INTO admin_keys (key_hash, label, created_at) VALUES (?, ?, ?)",
            params![hash, label, now],
        )?;
        Ok(key)
    }

    /// Verifies a long-lived admin key and records its last use. Revoked or
    /// unknown keys return the same error.
    pub fn authenticate_admin_key(&self, raw_key: &str, now: i64) -> Result<()> {
        let hash = auth::hash_token(raw_key.trim());
        let changed = self.conn.execute(
            "UPDATE admin_keys SET last_used_at = ?
              WHERE key_hash = ? AND revoked_at IS NULL",
            params![now, hash],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::Unauthenticated)
        }
    }

    pub fn revoke_admin_key(&self, id: i64, now: i64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE admin_keys SET revoked_at = ?
              WHERE id = ? AND revoked_at IS NULL",
            params![now, id],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::NotFound(format!("admin key {id}")))
        }
    }

    pub fn list_admin_keys(&self) -> Result<Vec<AdminKey>> {
        let mut statement = self.conn.prepare(
            "SELECT id, label, created_at, last_used_at, revoked_at
               FROM admin_keys ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AdminKey {
                id: row.get(0)?,
                label: row.get(1)?,
                created_at: row.get(2)?,
                last_used_at: row.get(3)?,
                revoked_at: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
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
            self.get_token_identity(token_id)?;
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

    pub fn count_grants_for_source(&self, source_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM tool_grants WHERE source_id = ?",
            [source_id],
            |row| row.get(0),
        )?)
    }

    /// Archive keeps every local record and never touches the physical database.
    pub fn archive_source(&self, id: &str, actor: &str, now: i64) -> Result<Source> {
        let transaction = self.conn.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE sources SET status = 'archived', archived_at = ?, updated_at = ?
              WHERE id = ? AND status = 'active'",
            params![now, now, id],
        )?;
        if changed == 0 {
            let status: Option<String> = transaction
                .query_row("SELECT status FROM sources WHERE id = ?", [id], |row| {
                    row.get(0)
                })
                .optional()?;
            return match status {
                None => Err(Error::NotFound(format!("source {id}"))),
                Some(_) => Err(Error::Conflict("source is not active".to_string())),
            };
        }
        let stale = approval_rows(
            &transaction,
            "WHERE source_id = ? AND status IN ('pending', 'approved')",
            [id],
        )?;
        for record in &stale {
            transaction.execute(
                "UPDATE approval_requests SET status = 'expired', reason_code = 'SOURCE_ARCHIVED'
                  WHERE id = ? AND status IN ('pending', 'approved')",
                [record.id],
            )?;
            let record = approval_from_conn(&transaction, record.id)?;
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(&record, "invalidated", "rejected", actor, now),
            )?;
        }
        insert_audit(
            &transaction,
            &source_event(id, "archived", actor, "SOURCE_ARCHIVED", now),
        )?;
        transaction.commit()?;
        self.get_source(id)
    }

    /// Sets the environment of an archived source. Archiving keeps the source disabled,
    /// so an admin can classify a migrated legacy source before restoring it.
    pub fn set_source_environment(&self, id: &str, environment: Environment) -> Result<Source> {
        let source = self.get_source(id)?;
        if source.status != SourceStatus::Archived {
            return Err(Error::Conflict(
                "source environment can only change while archived".to_string(),
            ));
        }
        if environment == Environment::Production {
            let write_grants: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM tool_grants
                  WHERE source_id = ?
                    AND tool IN ('mutate_sql', 'execute_ddl', 'create_database')",
                [id],
                |row| row.get(0),
            )?;
            if write_grants > 0 {
                return Err(Error::InvalidInput(
                    "cannot mark a source as production while it has write grants; revoke mutate_sql, execute_ddl and create_database first"
                        .to_string(),
                ));
            }
        }
        let changed = self.conn.execute(
            "UPDATE sources SET environment = ?, updated_at = ?
              WHERE id = ? AND status = 'archived'",
            params![environment.as_str(), now_ms(), id],
        )?;
        if changed == 0 {
            return Err(Error::Conflict(
                "source environment can only change while archived".to_string(),
            ));
        }
        self.get_source(id)
    }

    /// Restore re-checks profiles, grants and the provisioning quota before reactivating.
    pub fn restore_source(
        &self,
        id: &str,
        limits: &crate::provision::ProvisionLimits,
        actor: &str,
        now: i64,
    ) -> Result<Source> {
        let transaction = self.conn.unchecked_transaction()?;
        let source = self.get_source(id)?;
        if source.status != SourceStatus::Archived {
            return Err(Error::Conflict("source is not archived".to_string()));
        }
        let profiles = self.list_profiles(id)?;
        if !profiles.iter().any(|profile| profile.enabled) {
            return Err(Error::InvalidInput(
                "archived source has no enabled profile".to_string(),
            ));
        }
        if self.count_grants_for_source(id)? == 0 {
            return Err(Error::InvalidInput(
                "archived source has no grants to restore".to_string(),
            ));
        }
        if let Some(reservation) = self.reservation_by_database(id)? {
            if reservation.status != ReservationStatus::Committed {
                return Err(Error::Conflict(
                    "provisioned source has no committed reservation".to_string(),
                ));
            }
            self.get_token_identity(&reservation.token_id)?;
            let parent = self.get_source(&reservation.parent_source_id)?;
            if parent.status != SourceStatus::Active {
                return Err(Error::ProvisionBlocked(
                    "parent source is archived".to_string(),
                ));
            }
            let usage = self.provision_usage(
                &reservation.token_id,
                &reservation.parent_source_id,
                now,
                limits.min_interval_ms(),
            )?;
            if usage.active >= i64::from(limits.max_active()) {
                return Err(Error::ResourceLimit(
                    "restoring this source would exceed the provisioning quota".to_string(),
                ));
            }
        }
        let changed = transaction.execute(
            "UPDATE sources SET status = 'active', archived_at = NULL, updated_at = ?
              WHERE id = ? AND status = 'archived'",
            params![now, id],
        )?;
        if changed == 0 {
            return Err(Error::Conflict("source is not archived".to_string()));
        }
        insert_audit(
            &transaction,
            &source_event(id, "restored", actor, "SOURCE_RESTORED", now),
        )?;
        let restored = self.get_source(id)?;
        transaction.commit()?;
        Ok(restored)
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
        // The provision admin credential must never be reachable from an ordinary profile.
        let provision_namespace = profile.credential_ref.starts_with("keyring:provision/");
        if (profile.purpose == ProfilePurpose::Provision) != provision_namespace {
            return Err(Error::InvalidInput(
                "provision profiles must use keyring:provision/ and other profiles must not"
                    .to_string(),
            ));
        }
        crate::postgres::validate_limits(profile.max_connections, profile.statement_timeout_ms)?;
        let duplicate: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM connection_profiles WHERE source_id = ? AND name = ?",
            params![profile.source_id, profile.name],
            |row| row.get(0),
        )?;
        if duplicate > 0 {
            return Err(Error::Conflict("该数据源已存在同名连接配置。".to_string()));
        }
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

    /// Updates mutable profile fields and bumps `version`. Purpose is immutable.
    pub fn update_profile(
        &self,
        source_id: &str,
        id: i64,
        update: &ProfileUpdate,
    ) -> Result<Profile> {
        let source = self.get_source(source_id)?;
        if source.status != SourceStatus::Active {
            return Err(Error::InvalidInput(
                "归档数据源上的连接配置不能修改".to_string(),
            ));
        }
        let current = self.get_profile(id)?;
        if current.source_id != source_id {
            return Err(Error::NotFound(format!("profile {id}")));
        }
        validate_profile_name(&update.name)?;
        if update.name != current.name {
            let duplicate: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM connection_profiles
                  WHERE source_id = ? AND name = ? AND id <> ?",
                params![source_id, update.name, id],
                |row| row.get(0),
            )?;
            if duplicate > 0 {
                return Err(Error::Conflict(format!(
                    "数据源 {source_id} 上已存在同名连接配置：{}",
                    update.name
                )));
            }
        }
        let credential_ref = update
            .credential_ref
            .clone()
            .unwrap_or_else(|| current.credential_ref.clone());
        validate_credential_ref(&credential_ref)?;
        let provision_namespace = credential_ref.starts_with("keyring:provision/");
        if (current.purpose == ProfilePurpose::Provision) != provision_namespace {
            return Err(Error::InvalidInput(
                "provision profiles must use keyring:provision/ and other profiles must not"
                    .to_string(),
            ));
        }
        crate::postgres::validate_limits(update.max_connections, update.statement_timeout_ms)?;
        let changed = self.conn.execute(
            "UPDATE connection_profiles
                SET name = ?, credential_ref = ?, enabled = ?,
                    max_connections = ?, statement_timeout_ms = ?,
                    version = version + 1, updated_at = ?
              WHERE id = ? AND source_id = ?",
            params![
                update.name,
                credential_ref,
                if update.enabled { 1 } else { 0 },
                update.max_connections.map(i64::from),
                update.statement_timeout_ms.map(|value| value as i64),
                now_ms(),
                id,
                source_id
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("profile {id}")));
        }
        self.get_profile(id)
    }

    /// Deletes an unused profile. Referenced profiles must be detached first so a
    /// route or an in-flight approval can never point at a missing connection.
    pub fn delete_profile(&self, source_id: &str, id: i64) -> Result<()> {
        let profile = self.get_profile(id)?;
        if profile.source_id != source_id {
            return Err(Error::NotFound(format!("profile {id}")));
        }
        let routes: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM source_tool_routes WHERE profile_id = ?",
            [id],
            |row| row.get(0),
        )?;
        if routes > 0 {
            return Err(Error::Conflict(format!(
                "连接配置 {id} 仍被 {routes} 个工具路由引用，请先删除或改绑这些路由"
            )));
        }
        let approvals = active_approval_count_for(&self.conn, "profile_id", id)?;
        if approvals > 0 {
            return Err(Error::Conflict(format!(
                "连接配置 {id} 还有 {approvals} 个待批/已批请求，请先处理"
            )));
        }
        let changed = self.conn.execute(
            "DELETE FROM connection_profiles WHERE id = ? AND source_id = ?",
            params![id, source_id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("profile {id}")));
        }
        Ok(())
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
        if self.get_route(&route.source_id, route.tool).is_ok() {
            return Err(Error::Conflict(
                "这个数据源已经为该工具配置了工具路由。".to_string(),
            ));
        }
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

    pub fn list_routes(&self, source_id: &str) -> Result<Vec<Route>> {
        let mut statement = self.conn.prepare(
            "SELECT id, source_id, tool, profile_id, min_approval_policy, version
               FROM source_tool_routes WHERE source_id = ? ORDER BY tool",
        )?;
        let rows = statement
            .query_map([source_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, source_id, tool_text, profile_id, policy, version)| {
                Ok(Route {
                    id,
                    source_id,
                    tool: Tool::parse(&tool_text)?,
                    profile_id,
                    min_approval_policy: ApprovalPolicy::parse(&policy)?,
                    version,
                })
            })
            .collect()
    }

    pub fn get_route_by_id(&self, id: i64) -> Result<Route> {
        let row = self
            .conn
            .query_row(
                "SELECT id, source_id, tool, profile_id, min_approval_policy, version
                   FROM source_tool_routes WHERE id = ?",
                [id],
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
            return Err(Error::NotFound(format!("route {id}")));
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

    /// Updates a route's profile and minimum policy, bumping `version`. The tool
    /// itself is immutable so an approval can never be redirected to another tool.
    pub fn update_route(&self, source_id: &str, id: i64, update: &RouteUpdate) -> Result<Route> {
        let current = self.get_route_by_id(id)?;
        if current.source_id != source_id {
            return Err(Error::NotFound(format!("route {id}")));
        }
        let source = self.get_source(source_id)?;
        if source.status != SourceStatus::Active {
            return Err(Error::InvalidInput(
                "归档数据源上的工具路由不能修改".to_string(),
            ));
        }
        let profile = self.get_profile(update.profile_id)?;
        if profile.source_id != source_id {
            return Err(Error::InvalidInput(
                "route profile belongs to a different source".to_string(),
            ));
        }
        let Some(purpose) = current.tool.purpose() else {
            return Err(Error::InvalidInput(
                "list_sources is a discovery tool and cannot have a route".to_string(),
            ));
        };
        if purpose != profile.purpose {
            return Err(Error::InvalidInput(format!(
                "tool {} requires a {} profile",
                current.tool.as_str(),
                purpose.as_str()
            )));
        }
        validate_route_policy(current.tool, update.min_approval_policy)?;
        let changed = self.conn.execute(
            "UPDATE source_tool_routes
                SET profile_id = ?, min_approval_policy = ?,
                    version = version + 1, updated_at = ?
              WHERE id = ? AND source_id = ?",
            params![
                update.profile_id,
                update.min_approval_policy.as_str(),
                now_ms(),
                id,
                source_id
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("route {id}")));
        }
        self.get_route_by_id(id)
    }

    /// Deletes an unused route. Pending/approved requests still referencing it
    /// must be resolved first; failed claims would otherwise strand the admin UI.
    pub fn delete_route(&self, source_id: &str, id: i64) -> Result<()> {
        let route = self.get_route_by_id(id)?;
        if route.source_id != source_id {
            return Err(Error::NotFound(format!("route {id}")));
        }
        let approvals = active_approval_count_for(&self.conn, "route_id", id)?;
        if approvals > 0 {
            return Err(Error::Conflict(format!(
                "工具路由 {id} 还有 {approvals} 个待批/已批请求，请先处理"
            )));
        }
        let changed = self.conn.execute(
            "DELETE FROM source_tool_routes WHERE id = ? AND source_id = ?",
            params![id, source_id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("route {id}")));
        }
        Ok(())
    }

    pub fn create_grant(&self, grant: &NewGrant) -> Result<Grant> {
        self.get_token_identity(&grant.token_id)?;
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
        let exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tool_grants
              WHERE token_id = ? AND source_id = ? AND tool = ?",
            params![grant.token_id, grant.source_id, grant.tool.as_str()],
            |row| row.get(0),
        )?;
        if exists > 0 {
            return Err(Error::Conflict(
                "该令牌已经拥有这个数据源与工具的授权，请直接调整已有授权，或先移除再重新添加。"
                    .to_string(),
            ));
        }
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

    /// Removes one grant so the next call is denied without re-issuing the token.
    pub fn revoke_grant(&self, token_id: &str, source_id: &str, tool: Tool) -> Result<()> {
        let changed = self.conn.execute(
            "DELETE FROM tool_grants WHERE token_id = ? AND source_id = ? AND tool = ?",
            params![token_id, source_id, tool.as_str()],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!(
                "grant {source_id}/{}",
                tool.as_str()
            )));
        }
        Ok(())
    }

    /// Sources with at least one grant and not archived. Used for discovery only.
    pub fn list_visible_sources(&self, token_id: &str) -> Result<Vec<Source>> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT s.id
               FROM tool_grants g JOIN sources s ON s.id = g.source_id
              WHERE g.token_id = ? AND s.status = 'active'
              ORDER BY s.id",
        )?;
        let ids = statement
            .query_map([token_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| self.get_source(&id)).collect()
    }

    /// Every source, including archived ones. Used by the admin UI and CLI.
    pub fn list_all_sources(&self) -> Result<Vec<Source>> {
        let mut statement = self.conn.prepare("SELECT id FROM sources ORDER BY id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| self.get_source(&id)).collect()
    }

    /// Distinct tools granted on active sources. Visibility does not authorize a call.
    pub fn list_granted_tools(&self, token_id: &str) -> Result<Vec<Tool>> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT g.tool
               FROM tool_grants g JOIN sources s ON s.id = g.source_id
              WHERE g.token_id = ? AND s.status = 'active'",
        )?;
        let rows = statement
            .query_map([token_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|tool| Tool::parse(&tool)).collect()
    }

    pub fn insert_approval_request(&self, request: &NewApprovalRequest) -> Result<ApprovalRecord> {
        if request.channel == ApprovalChannel::Elicitation
            && (request.draft.high_risk || request.draft.tool == Tool::CreateDatabase)
        {
            return Err(Error::InvalidInput(
                "high-risk and create_database approvals require admin".to_string(),
            ));
        }
        if request.channel == ApprovalChannel::Elicitation
            && request.status == ApprovalStatus::Pending
        {
            return Err(Error::InvalidInput(
                "elicitation cannot leave a pending admin request".to_string(),
            ));
        }
        if !matches!(
            request.draft.tool,
            Tool::MutateSql | Tool::ExecuteDdl | Tool::CreateDatabase
        ) {
            return Err(Error::InvalidInput(
                "tool does not support approvals".to_string(),
            ));
        }
        let draft = &request.draft;
        let digest = draft.digest();
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO approval_requests
               (token_id, agent, source_id, source_environment, tool, sql, params_json,
                request_digest, profile_id, profile_version, route_id, route_version,
                grant_policy, route_min_policy, high_risk, channel, status, created_at,
                expires_at, decided_at, decided_by, reason_code, reason)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                draft.token_id,
                draft.agent,
                draft.source_id,
                draft.source_environment.as_str(),
                draft.tool.as_str(),
                draft.sql,
                draft.params_json,
                digest,
                draft.profile_id,
                draft.profile_version,
                draft.route_id,
                draft.route_version,
                draft.grant_policy.as_str(),
                draft.route_min_policy.as_str(),
                i64::from(draft.high_risk),
                request.channel.as_str(),
                request.status.as_str(),
                request.created_at,
                request.expires_at,
                request.decided_at,
                request.decided_by,
                request.reason_code,
                request.reason,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        let record = approval_from_conn(&transaction, id)?;
        insert_audit(
            &transaction,
            &AuditEvent::for_approval(
                &record,
                "requested",
                "pending",
                "system",
                request.created_at,
            ),
        )?;
        if record.status != ApprovalStatus::Pending {
            let decision = if record.status == ApprovalStatus::Approved {
                "allowed"
            } else {
                "rejected"
            };
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(
                    &record,
                    record.status.as_str(),
                    decision,
                    request.decided_by.as_deref().unwrap_or("mcp-client"),
                    request.decided_at.unwrap_or(request.created_at),
                ),
            )?;
        }
        transaction.commit()?;
        Ok(record)
    }

    pub fn get_approval(&self, id: i64) -> Result<ApprovalRecord> {
        approval_from_conn(&self.conn, id)
    }

    pub fn list_pending_approvals(&self, limit: usize) -> Result<Vec<ApprovalRecord>> {
        let mut statement = self.conn.prepare(
            "SELECT id FROM approval_requests WHERE status = 'pending' ORDER BY created_at LIMIT ?",
        )?;
        let ids = statement
            .query_map([limit as i64], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| approval_from_conn(&self.conn, id))
            .collect()
    }

    pub fn decide_approval(
        &self,
        id: i64,
        approve: bool,
        actor: &str,
        now: i64,
        execution_ttl_ms: i64,
    ) -> Result<DecisionOutcome> {
        let transaction = self.conn.unchecked_transaction()?;
        let Some(record) = approval_optional(&transaction, id)? else {
            transaction.commit()?;
            return Ok(DecisionOutcome::NotFound);
        };
        if record.status != ApprovalStatus::Pending {
            transaction.commit()?;
            return Ok(DecisionOutcome::AlreadyDecided);
        }
        if record.expires_at <= now {
            transaction.execute(
                "UPDATE approval_requests SET status = 'expired', decided_at = ?, reason_code = 'APPROVAL_EXPIRED'
                  WHERE id = ? AND status = 'pending'",
                params![now, id],
            )?;
            let record = approval_from_conn(&transaction, id)?;
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(&record, "expired", "expired", "system", now),
            )?;
            transaction.commit()?;
            return Ok(DecisionOutcome::Expired);
        }
        let status = if approve {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Denied
        };
        let changed = transaction.execute(
            "UPDATE approval_requests
                SET status = ?, decided_at = ?, decided_by = ?,
                    expires_at = CASE WHEN ? THEN ? ELSE expires_at END
              WHERE id = ? AND status = 'pending'",
            params![
                status.as_str(),
                now,
                actor,
                i64::from(approve),
                now + execution_ttl_ms,
                id
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(DecisionOutcome::AlreadyDecided);
        }
        let record = approval_from_conn(&transaction, id)?;
        insert_audit(
            &transaction,
            &AuditEvent::for_approval(
                &record,
                status.as_str(),
                if approve { "allowed" } else { "rejected" },
                actor,
                now,
            ),
        )?;
        transaction.commit()?;
        Ok(if approve {
            DecisionOutcome::Approved
        } else {
            DecisionOutcome::Denied
        })
    }

    pub fn claim_approval(
        &self,
        id: i64,
        token_id: &str,
        expected_digest: &str,
        now: i64,
    ) -> Result<ClaimOutcome> {
        let transaction = self.conn.unchecked_transaction()?;
        let outcome = match claim_step(&transaction, id, token_id, expected_digest, now)? {
            ClaimStep::Outcome(outcome) => outcome,
            ClaimStep::Claimed(record) => ClaimOutcome::Claimed(record),
        };
        transaction.commit()?;
        Ok(outcome)
    }

    /// Claims a provisioning approval and reserves quota in the same transaction, so a
    /// concurrent request either gets both or neither.
    pub fn claim_provision(
        &self,
        id: i64,
        token_id: &str,
        expected_digest: &str,
        database_name: &str,
        limits: &crate::provision::ProvisionLimits,
        now: i64,
    ) -> Result<ClaimOutcome> {
        let transaction = self.conn.unchecked_transaction()?;
        match claim_step(&transaction, id, token_id, expected_digest, now)? {
            ClaimStep::Outcome(outcome) => {
                transaction.commit()?;
                Ok(outcome)
            }
            ClaimStep::Claimed(record) => {
                let reservation = reserve_in_conn(
                    &transaction,
                    token_id,
                    &record.draft.source_id,
                    database_name,
                    Some(id),
                    limits,
                    now,
                )?;
                insert_audit(
                    &transaction,
                    &reservation_event(&reservation, "reserved", "allowed", "QUOTA_RESERVED", now),
                )?;
                transaction.commit()?;
                Ok(ClaimOutcome::Claimed(record))
            }
        }
    }

    pub fn finish_approval(
        &self,
        id: i64,
        outcome: ApprovalOutcome,
        row_count: Option<u64>,
        error_code: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        let transaction = self.conn.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE approval_requests
                SET outcome = ?, finished_at = ?, row_count = ?, error_code = ?
              WHERE id = ? AND status = 'consumed' AND outcome IS NULL",
            params![
                outcome.as_str(),
                now,
                row_count.map(|value| value as i64),
                error_code,
                id
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        let record = approval_from_conn(&transaction, id)?;
        let event = match outcome {
            ApprovalOutcome::Succeeded => "succeeded",
            ApprovalOutcome::Failed => "failed",
            ApprovalOutcome::Unknown => "unknown",
        };
        let mut audit = AuditEvent::for_approval(
            &record,
            event,
            match outcome {
                ApprovalOutcome::Succeeded => "allowed",
                ApprovalOutcome::Failed => "error",
                ApprovalOutcome::Unknown => "unknown",
            },
            "system",
            now,
        );
        audit.row_count = row_count.map(|value| value as i64);
        audit.error_code = error_code.map(str::to_string);
        insert_audit(&transaction, &audit)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn expire_due_approvals(&self, now: i64) -> Result<usize> {
        let transaction = self.conn.unchecked_transaction()?;
        let rows = approval_rows(
            &transaction,
            "WHERE status IN ('pending', 'approved') AND expires_at <= ?",
            params![now],
        )?;
        let mut count = 0;
        for record in rows {
            transaction.execute(
                "UPDATE approval_requests SET status = 'expired', reason_code = 'APPROVAL_EXPIRED'
                  WHERE id = ? AND status IN ('pending', 'approved')",
                [record.id],
            )?;
            let record = approval_from_conn(&transaction, record.id)?;
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(&record, "expired", "expired", "system", now),
            )?;
            count += 1;
        }
        transaction.commit()?;
        Ok(count)
    }

    pub fn recover_after_restart(&self, now: i64) -> Result<RecoverySummary> {
        let transaction = self.conn.unchecked_transaction()?;
        let pending = approval_rows(&transaction, "WHERE status = 'pending'", [])?;
        let approved = approval_rows(&transaction, "WHERE status = 'approved'", [])?;
        let inflight = approval_rows(
            &transaction,
            "WHERE status = 'consumed' AND outcome IS NULL",
            [],
        )?;

        for record in &pending {
            transaction.execute(
                "UPDATE approval_requests SET status = 'expired', reason_code = 'RESTART_PENDING'
                  WHERE id = ? AND status = 'pending'",
                [record.id],
            )?;
        }
        for record in &approved {
            transaction.execute(
                "UPDATE approval_requests SET status = 'expired', reason_code = 'RESTART_APPROVED'
                  WHERE id = ? AND status = 'approved'",
                [record.id],
            )?;
        }
        for record in &inflight {
            transaction.execute(
                "UPDATE approval_requests
                    SET outcome = 'unknown', finished_at = ?, error_code = 'RESTART_UNKNOWN',
                        reason_code = 'RESTART_UNKNOWN'
                  WHERE id = ? AND status = 'consumed' AND outcome IS NULL",
                params![now, record.id],
            )?;
        }

        // A reservation with no recorded outcome may have created a database; isolate it
        // until an admin reconciles instead of silently releasing the quota slot.
        let reserved = reservation_rows(&transaction, "WHERE status = 'reserved'", [])?;
        for reservation in &reserved {
            transaction.execute(
                "UPDATE quota_reservations
                    SET status = 'quarantined', settled_at = ?, reason_code = 'RESTART_UNKNOWN'
                  WHERE id = ? AND status = 'reserved'",
                params![now, reservation.id],
            )?;
        }

        for record in pending.iter().chain(approved.iter()) {
            let record = approval_from_conn(&transaction, record.id)?;
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(&record, "expired", "expired", "system", now),
            )?;
        }
        for record in &inflight {
            let record = approval_from_conn(&transaction, record.id)?;
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(&record, "unknown", "unknown", "system", now),
            )?;
        }
        for reservation in &reserved {
            insert_audit(
                &transaction,
                &reservation_event(
                    reservation,
                    "quarantined",
                    "unknown",
                    "RESTART_UNKNOWN",
                    now,
                ),
            )?;
        }
        transaction.commit()?;
        Ok(RecoverySummary {
            pending_expired: pending.len(),
            approved_expired: approved.len(),
            unknown_marked: inflight.len(),
        })
    }

    pub fn invalidate_approvals_for_token(
        &self,
        token_id: &str,
        reason_code: &str,
        now: i64,
    ) -> Result<usize> {
        let transaction = self.conn.unchecked_transaction()?;
        let rows = approval_rows(
            &transaction,
            "WHERE token_id = ? AND status IN ('pending', 'approved')",
            [token_id],
        )?;
        let mut count = 0;
        for record in rows {
            transaction.execute(
                "UPDATE approval_requests SET status = 'expired', reason_code = ?
                  WHERE id = ? AND status IN ('pending', 'approved')",
                params![reason_code, record.id],
            )?;
            let record = approval_from_conn(&transaction, record.id)?;
            insert_audit(
                &transaction,
                &AuditEvent::for_approval(&record, "invalidated", "rejected", "system", now),
            )?;
            count += 1;
        }
        transaction.commit()?;
        Ok(count)
    }

    /// Non-reserving quota view, used before an approval request is created.
    pub fn provision_usage(
        &self,
        token_id: &str,
        parent_source_id: &str,
        now: i64,
        min_interval_ms: i64,
    ) -> Result<ProvisionUsage> {
        usage_from_conn(&self.conn, token_id, parent_source_id, now, min_interval_ms)
    }

    /// Atomically re-checks quota and records a reservation before any external creation.
    pub fn reserve_provision(
        &self,
        token_id: &str,
        parent_source_id: &str,
        database_name: &str,
        approval_id: Option<i64>,
        limits: &crate::provision::ProvisionLimits,
        now: i64,
    ) -> Result<QuotaReservation> {
        validate_source_id(database_name)?;
        self.get_token_identity(token_id)?;
        let parent = self.get_source(parent_source_id)?;
        if parent.status != SourceStatus::Active {
            return Err(Error::ProvisionBlocked(
                "parent source is archived".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let reservation = reserve_in_conn(
            &transaction,
            token_id,
            parent_source_id,
            database_name,
            approval_id,
            limits,
            now,
        )?;
        transaction.commit()?;
        Ok(reservation)
    }

    /// Settles a reservation. Admin reconciliation may move a quarantined slot to
    /// committed or released, but settled slots never return to reserved.
    pub fn settle_reservation(
        &self,
        id: i64,
        target: ReservationStatus,
        reason_code: Option<&str>,
        now: i64,
    ) -> Result<QuotaReservation> {
        if target == ReservationStatus::Committed {
            let reservation = self.get_reservation(id)?;
            if !exists(
                &self.conn,
                "SELECT 1 FROM sources WHERE id = ? AND status = 'active'",
                [&reservation.database_name],
            )? {
                return Err(Error::InvalidInput(
                    "cannot commit a reservation without a registered active source".to_string(),
                ));
            }
        }
        let previous = match target {
            ReservationStatus::Committed | ReservationStatus::Released => "reserved",
            ReservationStatus::Quarantined => "reserved",
            ReservationStatus::Reserved => {
                return Err(Error::InvalidInput(
                    "reservation cannot return to reserved".to_string(),
                ))
            }
        };
        let alternative = match target {
            ReservationStatus::Committed | ReservationStatus::Released => Some("quarantined"),
            _ => None,
        };
        let changed = match alternative {
            Some(alternative) => self.conn.execute(
                "UPDATE quota_reservations SET status = ?, settled_at = ?, reason_code = ?
                  WHERE id = ? AND status IN (?, ?)",
                params![target.as_str(), now, reason_code, id, previous, alternative],
            )?,
            None => self.conn.execute(
                "UPDATE quota_reservations SET status = ?, settled_at = ?, reason_code = ?
                  WHERE id = ? AND status = ?",
                params![target.as_str(), now, reason_code, id, previous],
            )?,
        };
        if changed == 0 {
            return Err(Error::Conflict(
                "reservation is not in a settleable state".to_string(),
            ));
        }
        self.get_reservation(id)
    }

    /// Registers a created database atomically: source, profiles, routes, result grants
    /// and the reservation commitment. Any failure rolls back every local row.
    pub fn register_provisioned_source(
        &self,
        input: &crate::state::model::ProvisionedSource,
        reservation_id: i64,
        now: i64,
    ) -> Result<Source> {
        let reservation = self.get_reservation(reservation_id)?;
        if reservation.status != ReservationStatus::Reserved {
            return Err(Error::Conflict("reservation is not reserved".to_string()));
        }
        if reservation.database_name != input.source.id {
            return Err(Error::Conflict(
                "reservation does not match the database name".to_string(),
            ));
        }
        if input.source.created_token_id.as_deref() != Some(reservation.token_id.as_str()) {
            return Err(Error::InvalidInput(
                "provisioned source must record the creating token".to_string(),
            ));
        }
        if input.source.parent_source_id.as_deref() != Some(reservation.parent_source_id.as_str()) {
            return Err(Error::InvalidInput(
                "provisioned source must record the parent source".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let result = (|| -> Result<()> {
            self.create_source(&input.source)?;
            let mut profile_ids = std::collections::HashMap::new();
            for profile in &input.profiles {
                let created = self.create_profile(profile)?;
                profile_ids.insert(created.name, created.id);
            }
            for route in &input.routes {
                let profile_id = *profile_ids.get(&route.profile_name).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "route profile is not part of this source: {}",
                        route.profile_name
                    ))
                })?;
                self.create_route(&NewRoute {
                    source_id: input.source.id.clone(),
                    tool: route.tool,
                    profile_id,
                    min_approval_policy: route.min_approval_policy,
                })?;
            }
            for tool in &input.grants {
                self.create_grant(&NewGrant {
                    token_id: reservation.token_id.clone(),
                    source_id: input.source.id.clone(),
                    tool: *tool,
                    policy: None,
                })?;
            }
            let changed = self.conn.execute(
                "UPDATE quota_reservations
                    SET status = 'committed', settled_at = ?, reason_code = NULL
                  WHERE id = ? AND status = 'reserved'",
                params![now, reservation_id],
            )?;
            if changed == 0 {
                return Err(Error::Conflict(
                    "reservation was settled concurrently".to_string(),
                ));
            }
            Ok(())
        })();
        result?;
        transaction.commit()?;
        let committed = self.get_reservation(reservation_id)?;
        self.record_audit_event(&reservation_event(
            &committed,
            "registered",
            "allowed",
            "REGISTERED",
            now,
        ))?;
        self.get_source(&input.source.id)
    }

    pub fn reservation_for_approval(&self, approval_id: i64) -> Result<Option<QuotaReservation>> {
        Ok(reservation_rows(
            &self.conn,
            "WHERE approval_id = ? ORDER BY id DESC",
            [approval_id],
        )?
        .into_iter()
        .next())
    }

    pub fn reservation_by_database(&self, database_name: &str) -> Result<Option<QuotaReservation>> {
        Ok(reservation_rows(
            &self.conn,
            "WHERE database_name = ? ORDER BY id DESC",
            [database_name],
        )?
        .into_iter()
        .next())
    }

    pub fn list_recent_reservations(&self, limit: usize) -> Result<Vec<QuotaReservation>> {
        reservation_rows(
            &self.conn,
            "ORDER BY reserved_at DESC, id DESC LIMIT ?",
            [limit as i64],
        )
    }

    pub fn get_reservation(&self, id: i64) -> Result<QuotaReservation> {
        reservation_optional(&self.conn, id)?
            .ok_or_else(|| Error::NotFound(format!("quota reservation {id}")))
    }

    pub fn list_quarantined_reservations(&self) -> Result<Vec<QuotaReservation>> {
        reservation_rows(
            &self.conn,
            "WHERE status = 'quarantined' ORDER BY reserved_at, id",
            [],
        )
    }

    pub fn record_audit_event(&self, event: &AuditEvent) -> Result<()> {
        insert_audit(&self.conn, event)
    }

    pub fn count_audit_events(&self, approval_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE approval_id = ?",
            [approval_id],
            |row| row.get(0),
        )?)
    }

    pub fn count_audit_events_all(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM audit_events", [], |row| row.get(0))?)
    }

    pub fn count_pending_approvals(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM approval_requests WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?)
    }

    /// Newest audit events first; ties break on insertion order (newest id first).
    pub fn list_recent_audit(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        let mut statement = self.conn.prepare(
            "SELECT ts, event, approval_id, token_id, agent, source_id, tool, profile_id,
                    profile_version, route_id, route_version, request_digest, channel, decision,
                    actor, high_risk, reason_code, reason, duration_ms, row_count, error_code
               FROM audit_events ORDER BY ts DESC, id DESC LIMIT ?",
        )?;
        let rows = statement
            .query_map([limit as i64], audit_event_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Filtered audit page used by the local admin UI.
    pub fn query_audit(
        &self,
        agent: Option<&str>,
        source_id: Option<&str>,
        decision: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<AuditEvent>, i64)> {
        let mut filters = Vec::new();
        let mut values = Vec::<Value>::new();
        if let Some(agent) = agent {
            filters.push("agent = ?");
            values.push(Value::Text(agent.to_string()));
        }
        if let Some(source_id) = source_id {
            filters.push("source_id = ?");
            values.push(Value::Text(source_id.to_string()));
        }
        if let Some(decision) = decision {
            filters.push("decision = ?");
            values.push(Value::Text(decision.to_string()));
        }
        let where_clause = if filters.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", filters.join(" AND "))
        };
        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM audit_events{where_clause}"),
            params_from_iter(values.iter()),
            |row| row.get(0),
        )?;
        let mut page_values = values;
        page_values.push(Value::Integer(limit as i64));
        page_values.push(Value::Integer(offset as i64));
        let sql = format!(
            "SELECT ts, event, approval_id, token_id, agent, source_id, tool, profile_id,
                    profile_version, route_id, route_version, request_digest, channel, decision,
                    actor, high_risk, reason_code, reason, duration_ms, row_count, error_code
               FROM audit_events{where_clause}
              ORDER BY ts DESC, id DESC LIMIT ? OFFSET ?"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(page_values.iter()), audit_event_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((rows, total))
    }

    pub fn get_token_identity(&self, token_id: &str) -> Result<TokenIdentity> {
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

const APPROVAL_SELECT: &str =
    "SELECT id, token_id, agent, source_id, source_environment, tool, sql, params_json,
       request_digest, profile_id, profile_version, route_id, route_version,
       grant_policy, route_min_policy, high_risk, channel, status, created_at,
       expires_at, decided_at, decided_by, consumed_at, outcome, finished_at,
       row_count, error_code, reason_code, reason
  FROM approval_requests";

struct ApprovalRow {
    id: i64,
    token_id: String,
    agent: String,
    source_id: String,
    source_environment: String,
    tool: String,
    sql: Option<String>,
    params_json: String,
    request_digest: String,
    profile_id: i64,
    profile_version: i64,
    route_id: i64,
    route_version: i64,
    grant_policy: String,
    route_min_policy: String,
    high_risk: i64,
    channel: String,
    status: String,
    created_at: i64,
    expires_at: i64,
    decided_at: Option<i64>,
    decided_by: Option<String>,
    consumed_at: Option<i64>,
    outcome: Option<String>,
    finished_at: Option<i64>,
    row_count: Option<i64>,
    error_code: Option<String>,
    reason_code: Option<String>,
    reason: Option<String>,
}

impl ApprovalRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            token_id: row.get(1)?,
            agent: row.get(2)?,
            source_id: row.get(3)?,
            source_environment: row.get(4)?,
            tool: row.get(5)?,
            sql: row.get(6)?,
            params_json: row.get(7)?,
            request_digest: row.get(8)?,
            profile_id: row.get(9)?,
            profile_version: row.get(10)?,
            route_id: row.get(11)?,
            route_version: row.get(12)?,
            grant_policy: row.get(13)?,
            route_min_policy: row.get(14)?,
            high_risk: row.get(15)?,
            channel: row.get(16)?,
            status: row.get(17)?,
            created_at: row.get(18)?,
            expires_at: row.get(19)?,
            decided_at: row.get(20)?,
            decided_by: row.get(21)?,
            consumed_at: row.get(22)?,
            outcome: row.get(23)?,
            finished_at: row.get(24)?,
            row_count: row.get(25)?,
            error_code: row.get(26)?,
            reason_code: row.get(27)?,
            reason: row.get(28)?,
        })
    }

    fn into_record(self) -> Result<ApprovalRecord> {
        let draft = ApprovalDraft {
            token_id: self.token_id,
            agent: self.agent,
            source_id: self.source_id,
            source_environment: Environment::parse(&self.source_environment)?,
            tool: Tool::parse(&self.tool)?,
            sql: self.sql,
            params_json: self.params_json,
            profile_id: self.profile_id,
            profile_version: self.profile_version,
            route_id: self.route_id,
            route_version: self.route_version,
            grant_policy: ApprovalPolicy::parse(&self.grant_policy)?,
            route_min_policy: ApprovalPolicy::parse(&self.route_min_policy)?,
            high_risk: self.high_risk != 0,
        };
        let status = ApprovalStatus::parse(&self.status).ok_or_else(|| {
            Error::InvalidInput(format!("unknown approval status {}", self.status))
        })?;
        let channel = ApprovalChannel::parse(&self.channel).ok_or_else(|| {
            Error::InvalidInput(format!("unknown approval channel {}", self.channel))
        })?;
        let outcome =
            match self.outcome {
                Some(value) => Some(ApprovalOutcome::parse(&value).ok_or_else(|| {
                    Error::InvalidInput(format!("unknown approval outcome {value}"))
                })?),
                None => None,
            };
        Ok(ApprovalRecord {
            id: self.id,
            draft,
            request_digest: self.request_digest,
            channel,
            status,
            created_at: self.created_at,
            expires_at: self.expires_at,
            decided_at: self.decided_at,
            decided_by: self.decided_by,
            consumed_at: self.consumed_at,
            outcome,
            finished_at: self.finished_at,
            row_count: self.row_count.map(|value| value as u64),
            error_code: self.error_code,
            reason_code: self.reason_code,
            reason: self.reason,
        })
    }
}

fn approval_from_conn(conn: &Connection, id: i64) -> Result<ApprovalRecord> {
    approval_optional(conn, id)?.ok_or_else(|| Error::NotFound(format!("approval {id}")))
}

fn approval_optional(conn: &Connection, id: i64) -> Result<Option<ApprovalRecord>> {
    let sql = format!("{APPROVAL_SELECT} WHERE id = ?");
    let row = conn
        .query_row(&sql, [id], ApprovalRow::from_row)
        .optional()?;
    row.map(ApprovalRow::into_record).transpose()
}

fn approval_rows<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> Result<Vec<ApprovalRecord>> {
    let sql = format!("{APPROVAL_SELECT} {where_clause}");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params, ApprovalRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(ApprovalRow::into_record).collect()
}

const RESERVATION_SELECT: &str = "SELECT id, token_id, parent_source_id, database_name,
       approval_id, status, reserved_at, settled_at, reason_code
  FROM quota_reservations";

struct ReservationRow {
    id: i64,
    token_id: String,
    parent_source_id: String,
    database_name: String,
    approval_id: Option<i64>,
    status: String,
    reserved_at: i64,
    settled_at: Option<i64>,
    reason_code: Option<String>,
}

impl ReservationRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            token_id: row.get(1)?,
            parent_source_id: row.get(2)?,
            database_name: row.get(3)?,
            approval_id: row.get(4)?,
            status: row.get(5)?,
            reserved_at: row.get(6)?,
            settled_at: row.get(7)?,
            reason_code: row.get(8)?,
        })
    }

    fn into_reservation(self) -> Result<QuotaReservation> {
        Ok(QuotaReservation {
            id: self.id,
            token_id: self.token_id,
            parent_source_id: self.parent_source_id,
            database_name: self.database_name,
            approval_id: self.approval_id,
            status: ReservationStatus::parse(&self.status)?,
            reserved_at: self.reserved_at,
            settled_at: self.settled_at,
            reason_code: self.reason_code,
        })
    }
}

fn reservation_optional(conn: &Connection, id: i64) -> Result<Option<QuotaReservation>> {
    let sql = format!("{RESERVATION_SELECT} WHERE id = ?");
    conn.query_row(&sql, [id], ReservationRow::from_row)
        .optional()?
        .map(ReservationRow::into_reservation)
        .transpose()
}

fn reservation_rows<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> Result<Vec<QuotaReservation>> {
    let sql = format!("{RESERVATION_SELECT} {where_clause}");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params, ReservationRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(ReservationRow::into_reservation)
        .collect()
}

fn usage_from_conn(
    conn: &Connection,
    token_id: &str,
    parent_source_id: &str,
    now: i64,
    min_interval_ms: i64,
) -> Result<ProvisionUsage> {
    let window_start = now - min_interval_ms;
    let (active, recent, quarantined) = conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM quota_reservations q
              LEFT JOIN sources s ON s.id = q.database_name
             WHERE q.token_id = ?1 AND q.parent_source_id = ?2
               AND q.status IN ('reserved', 'committed')
               AND (q.status = 'reserved' OR s.status = 'active' OR s.id IS NULL)),
           (SELECT COUNT(*) FROM quota_reservations q
             WHERE q.token_id = ?1 AND q.parent_source_id = ?2
               AND q.status IN ('reserved', 'committed')
               AND COALESCE(q.settled_at, q.reserved_at) > ?3),
           (SELECT COUNT(*) FROM quota_reservations q
             WHERE q.token_id = ?1 AND q.parent_source_id = ?2 AND q.status = 'quarantined')",
        params![token_id, parent_source_id, window_start],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(ProvisionUsage {
        active,
        recent,
        quarantined,
    })
}

fn reserve_in_conn(
    conn: &Connection,
    token_id: &str,
    parent_source_id: &str,
    database_name: &str,
    approval_id: Option<i64>,
    limits: &crate::provision::ProvisionLimits,
    now: i64,
) -> Result<QuotaReservation> {
    if let Some(approval_id) = approval_id {
        let owner: Option<String> = conn
            .query_row(
                "SELECT token_id FROM approval_requests WHERE id = ?",
                [approval_id],
                |row| row.get(0),
            )
            .optional()?;
        match owner {
            Some(owner) if owner == token_id => {}
            Some(_) => {
                return Err(Error::Conflict(
                    "approval belongs to another token".to_string(),
                ))
            }
            None => return Err(Error::NotFound(format!("approval {approval_id}"))),
        }
    }
    if exists(
        conn,
        "SELECT 1 FROM quota_reservations WHERE database_name = ?",
        [database_name],
    )? {
        return Err(Error::Conflict(
            "database name is already reserved".to_string(),
        ));
    }
    let usage = usage_from_conn(
        conn,
        token_id,
        parent_source_id,
        now,
        limits.min_interval_ms(),
    )?;
    if usage.quarantined > 0 {
        return Err(Error::ProvisionBlocked(
            "an unsettled provisioning reservation blocks new databases for this token and parent"
                .to_string(),
        ));
    }
    if usage.active >= i64::from(limits.max_active()) {
        return Err(Error::ResourceLimit(
            "provision active database quota reached".to_string(),
        ));
    }
    if usage.recent > 0 {
        return Err(Error::ResourceLimit(
            "provision creation rate reached; retry after the interval".to_string(),
        ));
    }
    conn.execute(
        "INSERT INTO quota_reservations
           (token_id, parent_source_id, database_name, approval_id, status, reserved_at)
         VALUES (?, ?, ?, ?, 'reserved', ?)",
        params![token_id, parent_source_id, database_name, approval_id, now],
    )?;
    reservation_optional(conn, conn.last_insert_rowid())?
        .ok_or_else(|| Error::NotFound("quota reservation".to_string()))
}

fn reservation_event(
    reservation: &QuotaReservation,
    event: &str,
    decision: &str,
    reason_code: &str,
    now: i64,
) -> AuditEvent {
    AuditEvent {
        ts: now,
        event: event.to_string(),
        approval_id: reservation.approval_id,
        token_id: Some(reservation.token_id.clone()),
        agent: None,
        source_id: Some(reservation.parent_source_id.clone()),
        tool: Tool::CreateDatabase.as_str().to_string(),
        profile_id: None,
        profile_version: None,
        route_id: None,
        route_version: None,
        request_digest: None,
        channel: None,
        decision: decision.to_string(),
        actor: "system".to_string(),
        high_risk: true,
        reason_code: Some(reason_code.to_string()),
        reason: None,
        duration_ms: None,
        row_count: None,
        error_code: None,
    }
}

fn source_event(
    source_id: &str,
    event: &str,
    actor: &str,
    reason_code: &str,
    now: i64,
) -> AuditEvent {
    AuditEvent {
        ts: now,
        event: event.to_string(),
        approval_id: None,
        token_id: None,
        agent: None,
        source_id: Some(source_id.to_string()),
        tool: "source_lifecycle".to_string(),
        profile_id: None,
        profile_version: None,
        route_id: None,
        route_version: None,
        request_digest: None,
        channel: None,
        decision: if event == "restored" {
            "allowed"
        } else {
            "rejected"
        }
        .to_string(),
        actor: actor.to_string(),
        high_risk: false,
        reason_code: Some(reason_code.to_string()),
        reason: None,
        duration_ms: None,
        row_count: None,
        error_code: None,
    }
}

fn audit_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    Ok(AuditEvent {
        ts: row.get(0)?,
        event: row.get(1)?,
        approval_id: row.get(2)?,
        token_id: row.get(3)?,
        agent: row.get(4)?,
        source_id: row.get(5)?,
        tool: row.get(6)?,
        profile_id: row.get(7)?,
        profile_version: row.get(8)?,
        route_id: row.get(9)?,
        route_version: row.get(10)?,
        request_digest: row.get(11)?,
        channel: row.get(12)?,
        decision: row.get(13)?,
        actor: row.get(14)?,
        high_risk: row.get::<_, i64>(15)? != 0,
        reason_code: row.get(16)?,
        reason: row.get(17)?,
        duration_ms: row.get(18)?,
        row_count: row.get(19)?,
        error_code: row.get(20)?,
    })
}

fn insert_audit(conn: &Connection, event: &AuditEvent) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_events
           (ts, event, approval_id, token_id, agent, source_id, tool, profile_id,
            profile_version, route_id, route_version, request_digest, channel, decision,
            actor, high_risk, reason_code, reason, duration_ms, row_count, error_code)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            event.ts,
            event.event,
            event.approval_id,
            event.token_id,
            event.agent,
            event.source_id,
            event.tool,
            event.profile_id,
            event.profile_version,
            event.route_id,
            event.route_version,
            event.request_digest,
            event.channel,
            event.decision,
            event.actor,
            i64::from(event.high_risk),
            event.reason_code,
            event.reason,
            event.duration_ms,
            event.row_count,
            event.error_code,
        ],
    )?;
    Ok(())
}

enum ClaimStep {
    Outcome(ClaimOutcome),
    Claimed(Box<ApprovalRecord>),
}

/// Shared approval consumption logic. The caller owns the transaction so it can add
/// step-specific work (such as a quota reservation) before committing.
fn claim_step(
    conn: &Connection,
    id: i64,
    token_id: &str,
    expected_digest: &str,
    now: i64,
) -> Result<ClaimStep> {
    let Some(record) = approval_optional(conn, id)? else {
        return Ok(ClaimStep::Outcome(ClaimOutcome::NotFound));
    };
    if record.draft.token_id != token_id {
        return Ok(ClaimStep::Outcome(ClaimOutcome::TokenMismatch));
    }
    if record.request_digest != expected_digest || record.draft.digest() != expected_digest {
        return Ok(ClaimStep::Outcome(ClaimOutcome::DigestMismatch));
    }
    if record.status != ApprovalStatus::Approved {
        return Ok(ClaimStep::Outcome(ClaimOutcome::NotApproved));
    }
    if record.expires_at <= now {
        conn.execute(
            "UPDATE approval_requests SET status = 'expired', reason_code = 'APPROVAL_EXPIRED'
              WHERE id = ? AND status = 'approved'",
            [id],
        )?;
        let record = approval_from_conn(conn, id)?;
        insert_audit(
            conn,
            &AuditEvent::for_approval(&record, "expired", "expired", "system", now),
        )?;
        return Ok(ClaimStep::Outcome(ClaimOutcome::Expired));
    }
    if let Some(reason) = claim_stale_reason(conn, &record)? {
        conn.execute(
            "UPDATE approval_requests SET status = 'expired', reason_code = ?
              WHERE id = ? AND status = 'approved'",
            params![reason, id],
        )?;
        let record = approval_from_conn(conn, id)?;
        insert_audit(
            conn,
            &AuditEvent::for_approval(&record, "invalidated", "rejected", "system", now),
        )?;
        return Ok(ClaimStep::Outcome(ClaimOutcome::Stale(reason.to_string())));
    }
    let changed = conn.execute(
        "UPDATE approval_requests SET status = 'consumed', consumed_at = ?
          WHERE id = ? AND status = 'approved' AND consumed_at IS NULL",
        params![now, id],
    )?;
    if changed == 0 {
        return Ok(ClaimStep::Outcome(ClaimOutcome::NotApproved));
    }
    let record = approval_from_conn(conn, id)?;
    insert_audit(
        conn,
        &AuditEvent::for_approval(&record, "claimed", "allowed", "system", now),
    )?;
    Ok(ClaimStep::Claimed(Box::new(record)))
}

fn claim_stale_reason(conn: &Connection, record: &ApprovalRecord) -> Result<Option<&'static str>> {
    let draft = &record.draft;
    if !exists(
        conn,
        "SELECT 1 FROM tokens WHERE id = ? AND status = 'active'",
        [&draft.token_id],
    )? {
        return Ok(Some("TOKEN_REVOKED"));
    }
    if !exists(
        conn,
        "SELECT 1 FROM sources WHERE id = ? AND status = 'active' AND environment = ?",
        params![draft.source_id, draft.source_environment.as_str()],
    )? {
        return Ok(Some("SOURCE_ARCHIVED"));
    }
    if !exists(
        conn,
        "SELECT 1 FROM tool_grants
          WHERE token_id = ? AND source_id = ? AND tool = ? AND policy = ?",
        params![
            draft.token_id,
            draft.source_id,
            draft.tool.as_str(),
            draft.grant_policy.as_str()
        ],
    )? {
        return Ok(Some("GRANT_POLICY_CHANGED"));
    }
    if !exists(
        conn,
        "SELECT 1 FROM source_tool_routes
          WHERE id = ? AND source_id = ? AND tool = ? AND version = ?
            AND profile_id = ? AND min_approval_policy = ?",
        params![
            draft.route_id,
            draft.source_id,
            draft.tool.as_str(),
            draft.route_version,
            draft.profile_id,
            draft.route_min_policy.as_str()
        ],
    )? {
        return Ok(Some("ROUTE_CHANGED"));
    }
    let Some(purpose) = draft.tool.purpose() else {
        return Ok(Some("TOOL_CHANGED"));
    };
    if !exists(
        conn,
        "SELECT 1 FROM connection_profiles
          WHERE id = ? AND source_id = ? AND version = ? AND enabled = 1 AND purpose = ?",
        params![
            draft.profile_id,
            draft.source_id,
            draft.profile_version,
            purpose.as_str()
        ],
    )? {
        return Ok(Some("PROFILE_CHANGED"));
    }
    Ok(None)
}

fn exists<P: rusqlite::Params>(conn: &Connection, sql: &str, params: P) -> Result<bool> {
    Ok(conn
        .query_row(sql, params, |_| Ok(true))
        .optional()?
        .unwrap_or(false))
}

/// Pending or approved-but-not-yet-consumed approvals still hold a profile/route
/// reference; deleting the target would strand them.
fn active_approval_count_for(conn: &Connection, column: &str, id: i64) -> Result<i64> {
    let sql = match column {
        "profile_id" => {
            "SELECT COUNT(*) FROM approval_requests
              WHERE profile_id = ? AND status IN ('pending', 'approved')"
        }
        "route_id" => {
            "SELECT COUNT(*) FROM approval_requests
              WHERE route_id = ? AND status IN ('pending', 'approved')"
        }
        _ => {
            return Err(Error::InvalidInput(
                "unsupported approval reference column".to_string(),
            ))
        }
    };
    Ok(conn.query_row(sql, [id], |row| row.get(0))?)
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn v4_state_upgrades_without_losing_legacy_rows_or_foreign_keys() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("state.db");
        {
            let connection = Connection::open(&path).expect("create v4 state");
            connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (
                       version INTEGER PRIMARY KEY,
                       applied_at INTEGER NOT NULL
                     );",
                )
                .expect("schema_migrations");
            for migration in [MIGRATION_1, MIGRATION_2, MIGRATION_3, MIGRATION_4] {
                connection
                    .execute_batch(migration)
                    .expect("apply legacy migration");
            }
            connection
                .execute_batch(
                    "INSERT INTO schema_migrations (version, applied_at)
                       VALUES (1, 1), (2, 2), (3, 3), (4, 4);
                     INSERT INTO tokens (id, agent, token_hash, status, created_at)
                       VALUES ('tok-1', 'legacy-agent', 'hash-1', 'active', 1);
                     INSERT INTO sources
                       (id, driver, environment, status, created_at, updated_at)
                       VALUES ('legacy', 'postgres', 'nonproduction', 'active', 1, 1);
                     INSERT INTO connection_profiles
                       (source_id, name, purpose, credential_ref, created_at, updated_at)
                       VALUES ('legacy', 'query', 'query', 'keyring:legacy/query', 1, 1);
                     INSERT INTO tool_grants
                       (token_id, source_id, tool, policy, created_at)
                       VALUES ('tok-1', 'legacy', 'query_sql', 'direct', 1);
                     INSERT INTO approvals
                       (agent, source, tool, sql, params_json, status, channel,
                        created_at, expires_at, decided_at)
                       VALUES ('legacy-agent', 'legacy', 'query_sql', 'SELECT 1',
                               '[]', 'approved', 'admin', 1, 2, 1);
                     INSERT INTO audit
                       (ts, agent, source, tool, sql, params_json, duration_ms,
                        row_count, decision, channel, reason)
                       VALUES ('2026-01-01T00:00:00Z', 'legacy-agent', 'legacy',
                               'query_sql', 'SELECT 1', '[]', 5, 1, 'allowed',
                               'mcp-client', NULL);",
                )
                .expect("load legacy rows");
        }

        let store = Store::open(&path).expect("upgrade to v6");
        assert_eq!(store.schema_version().expect("version"), 6);
        assert_eq!(
            store.get_source("legacy").expect("legacy source").driver,
            Driver::Postgres
        );
        assert_eq!(store.count_grants_for_source("legacy").expect("grants"), 1);

        let sqlite = store
            .create_source(&NewSource {
                id: "sqlite-main".to_string(),
                driver: Driver::Sqlite,
                environment: Environment::NonProduction,
                parent_source_id: None,
                created_token_id: None,
                created_agent: None,
                created_origin: Some("test".to_string()),
            })
            .expect("sqlite source after upgrade");
        assert_eq!(sqlite.driver, Driver::Sqlite);
        store
            .create_grant(&NewGrant {
                token_id: "tok-1".to_string(),
                source_id: "sqlite-main".to_string(),
                tool: Tool::QuerySql,
                policy: None,
            })
            .expect("grant after upgrade");
        drop(store);

        let connection = Connection::open(&path).expect("inspect upgraded state");
        for (table, expected) in [("approvals", 1), ("audit", 1), ("sources", 2)] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("row count");
            assert_eq!(count, expected, "{table} rows changed");
        }
        let mut statement = connection
            .prepare("PRAGMA foreign_key_check")
            .expect("prepare foreign key check");
        let violations = statement
            .query_map([], |_| Ok(()))
            .expect("foreign key check")
            .count();
        assert_eq!(violations, 0, "foreign keys broken after migration");
    }

    #[test]
    fn t13_v5_state_without_sqlite_driver_is_repaired() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("state.db");
        {
            let connection = Connection::open(&path).expect("create v5 state");
            connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (
                       version INTEGER PRIMARY KEY,
                       applied_at INTEGER NOT NULL
                     );",
                )
                .expect("schema_migrations");
            for migration in [MIGRATION_1, MIGRATION_2, MIGRATION_3, MIGRATION_4] {
                connection
                    .execute_batch(migration)
                    .expect("apply legacy migration");
            }
            // The T-13 branch shipped admin_keys as v5 while the sources CHECK
            // still only accepted postgres.
            connection
                .execute_batch(MIGRATION_6)
                .expect("admin_keys table");
            connection
                .execute_batch(
                    "INSERT INTO schema_migrations (version, applied_at)
                       VALUES (1, 1), (2, 2), (3, 3), (4, 4), (5, 5);",
                )
                .expect("mark v5");
        }

        let store = Store::open(&path).expect("repair v5");
        assert_eq!(store.schema_version().expect("version"), 6);
        let sqlite = store
            .create_source(&NewSource {
                id: "sqlite-main".to_string(),
                driver: Driver::Sqlite,
                environment: Environment::NonProduction,
                parent_source_id: None,
                created_token_id: None,
                created_agent: None,
                created_origin: Some("test".to_string()),
            })
            .expect("sqlite source after v5 repair");
        assert_eq!(sqlite.driver, Driver::Sqlite);
        store
            .create_admin_key("laptop", 1_000)
            .expect("admin keys preserved");
    }
}
