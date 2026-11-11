import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { loadConfig, type AppConfig } from "../src/config.ts";
import { Approvals } from "../src/core/approval.ts";
import { Audit } from "../src/core/audit.ts";
import { PoolRegistry } from "../src/core/pools.ts";
import { Store } from "../src/core/store.ts";
import { startHttpServer, type RunningServer } from "../src/transport/http.ts";

let dir: string;
let dbPath: string;
let config: AppConfig;
let store: Store;
let pools: PoolRegistry;
let approvals: Approvals;
let running: RunningServer;
let baseUrl: string;
let client: Client;

const ADMIN = "http-admin-token";
const APPROVER = "http-approver-token";

function rowCount(): number {
  const db = new Database(dbPath);
  const row = db.query("SELECT COUNT(*) AS n FROM users").get() as { n: number };
  db.close();
  return row.n;
}

async function waitForPending(approvals: Approvals): Promise<number> {
  for (let i = 0; i < 200; i++) {
    const pending = approvals.listPending();
    if (pending.length > 0) return pending[0]!.id;
    await Bun.sleep(5);
  }
  throw new Error("no pending approval appeared");
}

beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "mcp-db-http-"));
  dbPath = join(dir, "test.db").replaceAll("\\", "/");
  const db = new Database(dbPath, { create: true });
  db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
  db.close();

  process.env.T_HTTP_ADMIN = ADMIN;
  process.env.T_HTTP_APPROVER = APPROVER;

  const configPath = join(dir, "mcp-db.toml");
  writeFileSync(
    configPath,
    `[server]
host = "127.0.0.1"
port = 0
admin_token_env = "T_HTTP_ADMIN"
state_path = "./state.db"
approval_timeout_ms = 3000

[[sources]]
id = "local"
driver = "sqlite"
dsn_readonly = "file:${dbPath}?mode=ro"
dsn_readwrite = "${dbPath}"

[agents.approver]
token_env = "T_HTTP_APPROVER"
sources = { local = "approval" }
`,
    "utf8",
  );

  config = loadConfig(configPath);
  store = new Store(config.server.statePath);
  const audit = new Audit(store);
  approvals = new Approvals(store, config.server.approvalTimeoutMs);
  pools = new PoolRegistry(config.sourceMap);
  running = await startHttpServer({ config, pools, approvals, audit });

  const address = running.httpServer.address();
  if (typeof address !== "object" || address === null) throw new Error("no server address");
  baseUrl = `http://127.0.0.1:${address.port}`;

  const transport = new StreamableHTTPClientTransport(new URL(`${baseUrl}/mcp`), {
    requestInit: { headers: { authorization: `Bearer ${APPROVER}` } },
  });
  client = new Client({ name: "http-test", version: "1.0.0" });
  await client.connect(transport);
});

afterAll(async () => {
  await client.close().catch(() => undefined);
  await running.close();
  await pools.closeAll();
  store.close();
  try {
    rmSync(dir, { recursive: true, force: true });
  } catch {
    // Windows can keep WAL files locked briefly; the temp dir is disposable.
  }
  delete process.env.T_HTTP_ADMIN;
  delete process.env.T_HTTP_APPROVER;
});

describe("HTTP auth", () => {
  test("rejects /mcp without a token", async () => {
    const response = await fetch(`${baseUrl}/mcp`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }),
    });
    expect(response.status).toBe(401);
  });

  test("rejects an agent token on the admin API", async () => {
    const response = await fetch(`${baseUrl}/admin/approvals`, {
      headers: { authorization: `Bearer ${APPROVER}` },
    });
    expect(response.status).toBe(401);
  });

  test("accepts the admin token on the admin API", async () => {
    const response = await fetch(`${baseUrl}/admin/approvals`, {
      headers: { authorization: `Bearer ${ADMIN}` },
    });
    expect(response.status).toBe(200);
  });
});

describe("HTTP MCP", () => {
  test("serves list_sources over Streamable HTTP", async () => {
    const result = await client.callTool({ name: "list_sources", arguments: {} });
    const data = JSON.parse((result.content as Array<{ text: string }>)[0]!.text) as {
      sources: Array<{ id: string; mode: string }>;
    };
    expect(data.sources[0]).toMatchObject({ id: "local", mode: "approval" });
  });

  test("approval blocks over HTTP until the admin API approves", async () => {
    const db = new Database(dbPath);
    db.exec("DELETE FROM users");
    db.close();

    const call = client.callTool({
      name: "execute_sql",
      arguments: { sql: "INSERT INTO users (name) VALUES ('http')" },
    });

    const id = await waitForPending(approvals);
    const response = await fetch(`${baseUrl}/admin/approvals/${id}/approve`, {
      method: "POST",
      headers: { authorization: `Bearer ${ADMIN}` },
    });
    expect(response.status).toBe(200);

    const result = await call;
    expect(result.isError).toBeFalsy();
    expect(rowCount()).toBe(1);
  });
});