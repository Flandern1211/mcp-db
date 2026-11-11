import type { Store } from "./store.ts";

export type AuditDecision = "allowed" | "rejected" | "error";

export interface AuditEntry {
  agent: string;
  source?: string | null;
  tool: string;
  sql?: string | null;
  paramsJson?: string | null;
  durationMs?: number | null;
  rowCount?: number | null;
  decision: AuditDecision;
  channel?: string | null;
  reason?: string | null;
}

/** Never log an unbounded statement/params blob. */
export const MAX_AUDIT_CHARS = 2000;

function clip(value: string | null | undefined): string | null {
  if (value == null) return null;
  return value.slice(0, MAX_AUDIT_CHARS);
}

/**
 * Audit trail for tool calls. Recording must never take the server down:
 * a broken state DB is a logging incident, not a reason to fail the query.
 */
export class Audit {
  private readonly store: Store;

  constructor(store: Store) {
    this.store = store;
  }

  record(entry: AuditEntry): void {
    try {
      this.store.recordAudit({
        ts: new Date().toISOString(),
        agent: entry.agent,
        source: entry.source ?? null,
        tool: entry.tool,
        sql: clip(entry.sql),
        paramsJson: clip(entry.paramsJson),
        durationMs: entry.durationMs ?? null,
        rowCount: entry.rowCount ?? null,
        decision: entry.decision,
        channel: entry.channel ?? null,
        reason: entry.reason ?? null,
      });
    } catch {
      // Logging failures must not surface to the agent.
    }
  }
}