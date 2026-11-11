import { Database } from "bun:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";

export type ApprovalStatus = "pending" | "approved" | "denied" | "expired";
export type ApprovalChannel = "elicitation" | "cli";

export interface ApprovalRecord {
  id: number;
  agent: string;
  source: string;
  tool: string;
  sql: string;
  paramsJson: string | null;
  status: ApprovalStatus;
  channel: ApprovalChannel;
  createdAt: number;
  expiresAt: number;
  decidedAt: number | null;
}

export interface NewApproval {
  agent: string;
  source: string;
  tool: string;
  sql: string;
  paramsJson: string | null;
  status: ApprovalStatus;
  channel: ApprovalChannel;
  createdAt: number;
  expiresAt: number;
  decidedAt: number | null;
}

export interface AuditRecord {
  id: number;
  ts: string;
  agent: string;
  source: string | null;
  tool: string;
  sql: string | null;
  paramsJson: string | null;
  durationMs: number | null;
  rowCount: number | null;
  decision: string;
  channel: string | null;
  reason: string | null;
}

export interface NewAudit {
  ts: string;
  agent: string;
  source: string | null;
  tool: string;
  sql: string | null;
  paramsJson: string | null;
  durationMs: number | null;
  rowCount: number | null;
  decision: string;
  channel: string | null;
  reason: string | null;
}

interface ApprovalRow {
  id: number;
  agent: string;
  source: string;
  tool: string;
  sql: string;
  params_json: string | null;
  status: string;
  channel: string;
  created_at: number;
  expires_at: number;
  decided_at: number | null;
}

interface AuditRow {
  id: number;
  ts: string;
  agent: string;
  source: string | null;
  tool: string;
  sql: string | null;
  params_json: string | null;
  duration_ms: number | null;
  row_count: number | null;
  decision: string;
  channel: string | null;
  reason: string | null;
}

const SCHEMA = `
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
`;

function toApproval(row: ApprovalRow): ApprovalRecord {
  return {
    id: row.id,
    agent: row.agent,
    source: row.source,
    tool: row.tool,
    sql: row.sql,
    paramsJson: row.params_json,
    status: row.status as ApprovalStatus,
    channel: row.channel as ApprovalChannel,
    createdAt: row.created_at,
    expiresAt: row.expires_at,
    decidedAt: row.decided_at,
  };
}

function toAudit(row: AuditRow): AuditRecord {
  return {
    id: row.id,
    ts: row.ts,
    agent: row.agent,
    source: row.source,
    tool: row.tool,
    sql: row.sql,
    paramsJson: row.params_json,
    durationMs: row.duration_ms,
    rowCount: row.row_count,
    decision: row.decision,
    channel: row.channel,
    reason: row.reason,
  };
}

/** Local state: pending CLI approvals and the audit trail. */
export class Store {
  private readonly db: Database;
  private readonly insertApprovalStmt;
  private readonly getApprovalStmt;
  private readonly listPendingStmt;
  private readonly decideStmt;
  private readonly expireStmt;
  private readonly insertAuditStmt;
  private readonly listAuditStmt;

  constructor(path: string) {
    mkdirSync(dirname(path), { recursive: true });
    this.db = new Database(path, { create: true });
    this.db.exec("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 3000;");
    this.db.exec(SCHEMA);

    this.insertApprovalStmt = this.db.prepare(
      `INSERT INTO approvals
        (agent, source, tool, sql, params_json, status, channel, created_at, expires_at, decided_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    this.getApprovalStmt = this.db.prepare("SELECT * FROM approvals WHERE id = ?");
    this.listPendingStmt = this.db.prepare(
      "SELECT * FROM approvals WHERE status = 'pending' ORDER BY created_at",
    );
    this.decideStmt = this.db.prepare(
      "UPDATE approvals SET status = ?, decided_at = ? WHERE id = ? AND status = 'pending'",
    );
    this.expireStmt = this.db.prepare(
      "UPDATE approvals SET status = 'expired', decided_at = ? WHERE status = 'pending' AND expires_at <= ?",
    );
    this.insertAuditStmt = this.db.prepare(
      `INSERT INTO audit
        (ts, agent, source, tool, sql, params_json, duration_ms, row_count, decision, channel, reason)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    this.listAuditStmt = this.db.prepare("SELECT * FROM audit ORDER BY id DESC LIMIT ?");
  }

  insertApproval(record: NewApproval): number {
    const info = this.insertApprovalStmt.run(
      record.agent,
      record.source,
      record.tool,
      record.sql,
      record.paramsJson,
      record.status,
      record.channel,
      record.createdAt,
      record.expiresAt,
      record.decidedAt,
    );
    return Number(info.lastInsertRowid);
  }

  getApproval(id: number): ApprovalRecord | null {
    const row = this.getApprovalStmt.get(id) as ApprovalRow | null;
    return row ? toApproval(row) : null;
  }

  listPending(): ApprovalRecord[] {
    return (this.listPendingStmt.all() as ApprovalRow[]).map(toApproval);
  }

  /** Returns false when the request is unknown or no longer pending. */
  decide(id: number, status: "approved" | "denied" | "expired", decidedAt = Date.now()): boolean {
    return this.decideStmt.run(status, decidedAt, id).changes > 0;
  }

  expirePending(now = Date.now()): number {
    return this.expireStmt.run(now, now).changes;
  }

  recordAudit(record: NewAudit): void {
    this.insertAuditStmt.run(
      record.ts,
      record.agent,
      record.source,
      record.tool,
      record.sql,
      record.paramsJson,
      record.durationMs,
      record.rowCount,
      record.decision,
      record.channel,
      record.reason,
    );
  }

  listAudit(limit = 50): AuditRecord[] {
    return (this.listAuditStmt.all(limit) as AuditRow[]).map(toAudit);
  }

  close(): void {
    this.db.close();
  }
}