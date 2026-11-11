import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Approvals } from "../src/core/approval.ts";
import { Store } from "../src/core/store.ts";

let dir: string;
let store: Store;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "mcp-db-store-"));
  store = new Store(join(dir, "state.db"));
});

afterEach(() => {
  store.close();
  try {
    rmSync(dir, { recursive: true, force: true });
  } catch {
    // Windows can keep WAL files locked briefly; the temp dir is disposable.
  }
});

const REQUEST = {
  agent: "gbrain",
  source: "local",
  tool: "execute_sql",
  sql: "UPDATE users SET name = 'a'",
  paramsJson: "[]",
};

describe("Store approvals", () => {
  test("creates, lists and decides a pending approval", () => {
    const id = store.insertApproval({
      ...REQUEST,
      status: "pending",
      channel: "cli",
      createdAt: 1,
      expiresAt: 10,
      decidedAt: null,
    });
    expect(store.listPending().map((row) => row.id)).toEqual([id]);
    expect(store.decide(id, "approved")).toBe(true);
    expect(store.decide(id, "denied")).toBe(false);
    expect(store.getApproval(id)?.status).toBe("approved");
    expect(store.listPending()).toHaveLength(0);
  });

  test("expires overdue pending rows", () => {
    const id = store.insertApproval({
      ...REQUEST,
      status: "pending",
      channel: "cli",
      createdAt: 1,
      expiresAt: 2,
      decidedAt: null,
    });
    expect(store.expirePending(3)).toBe(1);
    expect(store.getApproval(id)?.status).toBe("expired");
  });
});

describe("Store audit", () => {
  test("records and lists newest first", () => {
    store.recordAudit({
      ts: "2026-11-11T00:00:00.000Z",
      agent: "gbrain",
      source: "local",
      tool: "execute_sql",
      sql: "SELECT 1",
      paramsJson: "[]",
      durationMs: 3,
      rowCount: 1,
      decision: "allowed",
      channel: null,
      reason: null,
    });
    store.recordAudit({
      ts: "2026-11-11T00:00:01.000Z",
      agent: "gbrain",
      source: "local",
      tool: "execute_sql",
      sql: "UPDATE users SET name = 'a'",
      paramsJson: "[]",
      durationMs: null,
      rowCount: null,
      decision: "rejected",
      channel: "cli",
      reason: "denied by operator",
    });
    const rows = store.listAudit(10);
    expect(rows).toHaveLength(2);
    expect(rows[0]!.decision).toBe("rejected");
  });
});

describe("Approvals", () => {
  test("CLI decision resolves the blocked request", async () => {
    const approvals = new Approvals(store, 5_000);
    const pending = approvals.requestViaCli(REQUEST);
    const record = approvals.listPending()[0]!;
    expect(approvals.decide(record.id, true)).toBe("approved");
    await expect(pending).resolves.toEqual({ approved: true, channel: "cli", reason: undefined });
  });

  test("timeout resolves as a rejection and marks the row expired", async () => {
    const approvals = new Approvals(store, 20);
    const pending = approvals.requestViaCli(REQUEST);
    const record = approvals.listPending()[0]!;
    const decision = await pending;
    expect(decision.approved).toBe(false);
    expect(decision.reason).toBe("approval timed out");
    expect(approvals.get(record.id)?.status).toBe("expired");
  });

  test("elicitation decisions are recorded without a queue", () => {
    const approvals = new Approvals(store, 5_000);
    const decision = approvals.recordElicitation(REQUEST, false);
    expect(decision).toEqual({
      approved: false,
      channel: "elicitation",
      reason: "denied by user",
    });
    expect(approvals.listPending()).toHaveLength(0);
    expect(store.listAudit(10)).toHaveLength(0);
  });
});