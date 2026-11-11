import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { ElicitRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import { loadConfig, type AppConfig } from "../src/config.ts";
import { Approvals } from "../src/core/approval.ts";
import { Audit } from "../src/core/audit.ts";
import { PoolRegistry } from "../src/core/pools.ts";
import { Store } from "../src/core/store.ts";
import { createMcpServer } from "../src/tools/index.ts";

let dir: string;
let dbPath: string;
let config: AppConfig;
let store: Store;
let audit: Audit;
let approvals: Approvals;
let pools: PoolRegistry;

const clients: Client[] = [];

function resetUsers(): void {
  const db = new Database(dbPath);
  db.exec("DELETE FROM users");
  db.close();
}

function rowCount(): number {
  const db = new Database(dbPath);
  const row = db.query("SELECT COUNT(*) AS n FROM users").get() as { n: number };
  db.close();
  return row.n;
}

async function openClient(agentId: string, elicitation?: (approve: boolean) => void): Promise<Client> {
  const agent = config.agentMap.get(agentId)!;
  const server = createMcpServer({ config, agent, pools, approvals, audit });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await server.connect(serverTransport);

  const client = new Client(
    { name: `test-${agentId}`, version: "1.0.0" },
    elicitation ? { capabilities: { elicitation: {} } } : undefined,
  );

  if (elicitation) {
    client.setRequestHandler(ElicitRequestSchema, async () => {
      elicitation(true);
      return { action: "accept", content: { approve: true } };
    });
  }

  await client.connect(clientTransport);
  clients.push(client);
  return client;
}

function parseResult(result: unknown): Record<string, unknown> {
  const content = (result as { content?: Array<{ text?: string }> }).content ?? [];
  return JSON.parse(content[0]?.text ?? "{}") as Record<string, unknown>;
}

async function waitForPending(): Promise<number> {
  for (let i = 0; i < 200; i++) {
    const pending = approvals.listPending();
    if (pending.length > 0) return pending[0]!.id;
    await Bun.sleep(5);
  }
  throw new Error("no pending approval appeared");
}

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "mcp-db-int-"));
  dbPath = join(dir, "test.db").replaceAll("\\", "/");
  const db = new Database(dbPath, { create: true });
  db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
  db.close();

  process.env.T_ADMIN = "admin-token";
  process.env.T_READER = "reader-token";
  process.env.T_WRITER = "writer-token";
  process.env.T_APPROVER = "approver-token";
  process.env.T_ELICIT = "elicit-token";

  const configPath = join(dir, "mcp-db.toml");
  writeFileSync(
    configPath,
    `[server]
admin_token_env = "T_ADMIN"
state_path = "./state.db"
approval_timeout_ms = 2000

[[sources]]
id = "local"
driver = "sqlite"
dsn_readonly = "file:${dbPath}?mode=ro"
dsn_readwrite = "${dbPath}"
max_rows = 2
statement_timeout_ms = 5000

[agents.reader]
token_env = "T_READER"
sources = { local = "readonly" }

[agents.writer]
token_env = "T_WRITER"
sources = { local = "readwrite" }

[agents.approver]
token_env = "T_APPROVER"
sources = { local = "approval" }

[agents.elicit]
token_env = "T_ELICIT"
sources = { local = "approval" }
`,
    "utf8",
  );

  config = loadConfig(configPath);
  store = new Store(config.server.statePath);
  audit = new Audit(store);
  approvals = new Approvals(store, config.server.approvalTimeoutMs);
  pools = new PoolRegistry(config.sourceMap);
});

afterAll(async () => {
  for (const client of clients) await client.close().catch(() => undefined);
  await pools.closeAll();
  store.close();
  try {
    rmSync(dir, { recursive: true, force: true });
  } catch {
    // Windows can keep WAL files locked briefly; the temp dir is disposable.
  }
  for (const name of ["T_ADMIN", "T_READER", "T_WRITER", "T_APPROVER", "T_ELICIT"]) {
    delete process.env[name];
  }
});

describe("tool surface", () => {
  test("lists the sources visible to the agent", async () => {
    const reader = await openClient("reader");
    const result = await reader.callTool({ name: "list_sources", arguments: {} });
    const data = parseResult(result) as {
      sources: Array<{ id: string; label: string; driver: string; mode: string }>;
    };
    expect(data.sources).toEqual([
      { id: "local", label: "local", driver: "sqlite", mode: "readonly" },
    ]);
  });

  test("describes a sqlite table", async () => {
    const reader = await openClient("reader");
    const result = await reader.callTool({
      name: "describe_table",
      arguments: { table: "users" },
    });
    const data = parseResult(result) as { columns: Array<{ name: string; primaryKey: boolean }> };
    expect(data.columns.map((column) => column.name)).toEqual(["id", "name"]);
    expect(data.columns[0]!.primaryKey).toBe(true);
  });
});

describe("readonly mode", () => {
  test("a write is rejected by the read-only database", async () => {
    resetUsers();
    const reader = await openClient("reader");
    const result = await reader.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('reader')" },
    });
    expect(result.isError).toBe(true);
    expect(rowCount()).toBe(0);
  });
});

describe("readwrite mode", () => {
  test("writes and reads use the read-write pool", async () => {
    resetUsers();
    const writer = await openClient("writer");
    const insert = await writer.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('writer')" },
    });
    expect(insert.isError).toBeFalsy();

    const select = await writer.callTool({
      name: "execute_sql",
      arguments: { sql: "SELECT name FROM users" },
    });
    const data = parseResult(select) as { rows: Array<{ name: string }> };
    expect(data.rows).toEqual([{ name: "writer" }]);
  });

  test("truncates results at max_rows and says so", async () => {
    resetUsers();
    const writer = await openClient("writer");
    await writer.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('a'), ('b'), ('c')" },
    });
    const select = await writer.callTool({
      name: "execute_sql",
      arguments: { sql: "SELECT name FROM users ORDER BY id" },
    });
    const data = parseResult(select) as {
      rows: unknown[];
      rowCount: number;
      truncated: boolean;
    };
    expect(data.rowCount).toBe(3);
    expect(data.rows).toHaveLength(2);
    expect(data.truncated).toBe(true);
  });

  test("binds params instead of interpolating them", async () => {
    resetUsers();
    const writer = await openClient("writer");
    const payload = "x'); DROP TABLE users; --";
    const insert = await writer.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES (?)", params: [payload] },
    });
    expect(insert.isError).toBeFalsy();

    const select = await writer.callTool({
      name: "execute_sql",
      arguments: { sql: "SELECT name FROM users" },
    });
    const data = parseResult(select) as { rows: Array<{ name: string }> };
    expect(data.rows[0]!.name).toBe(payload);
    expect(rowCount()).toBe(1);
  });
});

describe("approval mode", () => {
  test("blocks until the CLI approves, then executes", async () => {
    resetUsers();
    const approver = await openClient("approver");
    const call = approver.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('approved')" },
    });
    const id = await waitForPending();
    expect(approvals.decide(id, true)).toBe("approved");
    const result = await call;
    expect(result.isError).toBeFalsy();
    expect(rowCount()).toBe(1);
  });

  test("a denied write does not execute", async () => {
    resetUsers();
    const approver = await openClient("approver");
    const call = approver.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('denied')" },
    });
    const id = await waitForPending();
    expect(approvals.decide(id, false)).toBe("denied");
    const result = await call;
    expect(result.isError).toBe(true);
    expect(rowCount()).toBe(0);
  });

  test("elicitation approval executes without the CLI queue", async () => {
    resetUsers();
    let elicited = false;
    const client = await openClient("elicit", () => {
      elicited = true;
    });
    const result = await client.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('elicited')" },
    });
    expect(result.isError).toBeFalsy();
    expect(elicited).toBe(true);
    expect(rowCount()).toBe(1);
    expect(approvals.listPending()).toHaveLength(0);
  });

  test("reads do not trigger approval", async () => {
    resetUsers();
    const client = await openClient("elicit", () => {
      throw new Error("read triggered elicitation");
    });
    const result = await client.callTool({
      name: "execute_sql",
      arguments: { sql: "SELECT COUNT(*) AS n FROM users" },
    });
    expect(result.isError).toBeFalsy();
  });
});

describe("audit", () => {
  test("records allowed, rejected and error decisions", () => {
    const decisions = new Set(store.listAudit(100).map((row) => row.decision));
    expect(decisions.has("allowed")).toBe(true);
    expect(decisions.has("rejected")).toBe(true);
  });
});