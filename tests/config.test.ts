import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadConfig } from "../src/config.ts";

let dir: string;

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "mcp-db-config-"));
  process.env.MCPDB_TEST_ADMIN = "admin-token";
  process.env.MCPDB_TEST_AGENT = "agent-token";
});

afterAll(() => {
  delete process.env.MCPDB_TEST_ADMIN;
  delete process.env.MCPDB_TEST_AGENT;
  rmSync(dir, { recursive: true, force: true });
});

function writeConfig(name: string, body: string): string {
  const path = join(dir, name);
  writeFileSync(path, body, "utf8");
  return path;
}

const VALID = `
[server]
admin_token_env = "MCPDB_TEST_ADMIN"
state_path = "./state.db"

[[sources]]
id = "local"
driver = "sqlite"
dsn_readonly = "file:./dev.db?mode=ro"
dsn_readwrite = "./dev.db"

[agents.gbrain]
token_env = "MCPDB_TEST_AGENT"
sources = { local = "approval" }
`;

describe("loadConfig", () => {
  test("loads per-source modes", () => {
    const config = loadConfig(writeConfig("ok.toml", VALID));
    const agent = config.agentMap.get("gbrain")!;
    expect(agent.sources.get("local")).toBe("approval");
    expect(config.server.host).toBe("127.0.0.1");
    expect(config.server.port).toBe(8787);
    expect(config.sources[0]!.driver).toBe("sqlite");
  });

  test("rejects unknown sources", () => {
    const path = writeConfig(
      "unknown-source.toml",
      VALID.replace('sources = { local = "approval" }', 'sources = { nope = "approval" }'),
    );
    expect(() => loadConfig(path)).toThrow(/unknown source "nope"/);
  });

  test("rejects non-loopback hosts", () => {
    const path = writeConfig("host.toml", VALID.replace('admin_token_env', 'host = "0.0.0.0"\nadmin_token_env'));
    expect(() => loadConfig(path)).toThrow(/must be 127\.0\.0\.1 or ::1/);
  });

  test("rejects a missing token env", () => {
    const path = writeConfig(
      "missing-token.toml",
      VALID.replace('token_env = "MCPDB_TEST_AGENT"', 'token_env = "MCPDB_TEST_MISSING"'),
    );
    expect(() => loadConfig(path)).toThrow(/MCPDB_TEST_MISSING/);
  });

  test("CLI mode skips agent tokens", () => {
    const path = writeConfig(
      "cli.toml",
      VALID.replace('token_env = "MCPDB_TEST_AGENT"', 'token_env = "MCPDB_TEST_MISSING"'),
    );
    expect(() => loadConfig(path, { requireAgentTokens: false })).not.toThrow();
  });
});