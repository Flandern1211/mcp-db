import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parse as parseToml } from "smol-toml";
import { z } from "zod";
import { hashToken } from "./auth.ts";
import type { DriverName } from "./drivers/types.ts";

export type WriteMode = "readonly" | "readwrite" | "approval";

export interface SourceConfig {
  id: string;
  driver: DriverName;
  dsnReadonly: string;
  dsnReadwrite: string;
  maxRows: number;
  statementTimeoutMs?: number;
  label: string;
}

export interface AgentConfig {
  id: string;
  tokenEnv: string;
  /** SHA-256 of the raw token. Empty when tokens were not resolved (CLI modes). */
  tokenHash: string;
  /** Per-source write mode. A source absent here is not accessible. */
  sources: Map<string, WriteMode>;
}

export interface ServerConfig {
  host: string;
  port: number;
  adminTokenEnv: string;
  /** Raw admin token, needed by the CLI to call the admin API. Never persisted. */
  adminToken: string;
  adminTokenHash: string;
  statePath: string;
  approvalTimeoutMs: number;
}

export interface AppConfig {
  configPath: string;
  server: ServerConfig;
  sources: SourceConfig[];
  sourceMap: Map<string, SourceConfig>;
  agents: AgentConfig[];
  agentMap: Map<string, AgentConfig>;
  /** Lookup by token hash; unknown tokens simply miss. */
  agentByTokenHash: Map<string, AgentConfig>;
}

const WriteModeSchema = z.enum(["readonly", "readwrite", "approval"]);

const ServerSchema = z.object({
  host: z.string().min(1).default("127.0.0.1"),
  port: z.number().int().min(0).max(65535).default(8787),
  admin_token_env: z.string().min(1),
  state_path: z.string().min(1).default("./.mcp-db/state.db"),
  approval_timeout_ms: z.number().int().min(1000).default(120_000),
});

const SourceSchema = z.object({
  id: z.string().min(1),
  driver: z.enum(["postgres", "mysql", "sqlite", "mssql"]),
  dsn_readonly: z.string().min(1),
  dsn_readwrite: z.string().min(1),
  max_rows: z.number().int().min(1).default(500),
  statement_timeout_ms: z.number().int().min(1).optional(),
  label: z.string().min(1).optional(),
});

const AgentSchema = z.object({
  token_env: z.string().min(1),
  sources: z.record(z.string(), WriteModeSchema).default({}),
});

const RootSchema = z.object({
  server: ServerSchema,
  sources: z.array(SourceSchema).default([]),
  agents: z.record(z.string(), AgentSchema).default({}),
});

export interface LoadConfigOptions {
  /** When false, agent token env vars are not read. Used by CLI commands that only talk to the admin API. */
  requireAgentTokens?: boolean;
}

/** The service is only meant to listen on loopback behind an SSH tunnel. */
function assertLoopbackHost(host: string): void {
  if (host !== "127.0.0.1" && host !== "::1") {
    throw new Error(
      `config: server.host must be 127.0.0.1 or ::1 (got "${host}"); expose the service through an SSH tunnel instead`,
    );
  }
}

function readTokenEnv(envName: string, context: string): string {
  const raw = process.env[envName];
  if (raw === undefined || raw.length === 0) {
    throw new Error(`config: environment variable ${envName} (${context}) is not set`);
  }
  return raw;
}

function formatZodError(error: z.ZodError): string {
  return error.issues
    .map((issue) => {
      const path = issue.path.length > 0 ? issue.path.join(".") : "(root)";
      return `${path}: ${issue.message}`;
    })
    .join("; ");
}

/**
 * Load and validate mcp-db.toml. Secrets stay in env vars; the config only
 * names the variables. Throws with a readable message on invalid input.
 */
export function loadConfig(path: string, options: LoadConfigOptions = {}): AppConfig {
  const requireAgentTokens = options.requireAgentTokens ?? true;
  const configPath = resolve(path);

  let text: string;
  try {
    text = readFileSync(configPath, "utf8");
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    throw new Error(`config: cannot read ${configPath}: ${message}`);
  }

  let parsed: unknown;
  try {
    parsed = parseToml(text);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    throw new Error(`config: invalid TOML in ${configPath}: ${message}`);
  }

  const result = RootSchema.safeParse(parsed);
  if (!result.success) {
    throw new Error(`config: invalid configuration: ${formatZodError(result.error)}`);
  }

  const root = result.data;
  assertLoopbackHost(root.server.host);

  const sourceList: SourceConfig[] = [];
  const sourceMap = new Map<string, SourceConfig>();
  for (const source of root.sources) {
    if (sourceMap.has(source.id)) {
      throw new Error(`config: duplicate source id "${source.id}"`);
    }
    const normalized: SourceConfig = {
      id: source.id,
      driver: source.driver,
      dsnReadonly: source.dsn_readonly,
      dsnReadwrite: source.dsn_readwrite,
      maxRows: source.max_rows,
      label: source.label ?? source.id,
      ...(source.statement_timeout_ms === undefined
        ? {}
        : { statementTimeoutMs: source.statement_timeout_ms }),
    };
    sourceMap.set(source.id, normalized);
    sourceList.push(normalized);
  }

  const adminToken = readTokenEnv(root.server.admin_token_env, "admin token");

  const agentList: AgentConfig[] = [];
  const agentMap = new Map<string, AgentConfig>();
  const agentByTokenHash = new Map<string, AgentConfig>();

  for (const [id, agent] of Object.entries(root.agents)) {
    const modes = new Map<string, WriteMode>();
    for (const [sourceId, mode] of Object.entries(agent.sources)) {
      if (!sourceMap.has(sourceId)) {
        throw new Error(`config: agent "${id}" references unknown source "${sourceId}"`);
      }
      modes.set(sourceId, mode);
    }

    let tokenHash = "";
    if (requireAgentTokens) {
      const raw = readTokenEnv(agent.token_env, `agent "${id}"`);
      tokenHash = hashToken(raw);
      const collision = agentByTokenHash.get(tokenHash);
      if (collision) {
        throw new Error(`config: agents "${collision.id}" and "${id}" have the same token`);
      }
    }

    const normalized: AgentConfig = {
      id,
      tokenEnv: agent.token_env,
      tokenHash,
      sources: modes,
    };
    agentMap.set(id, normalized);
    agentList.push(normalized);
    if (tokenHash) agentByTokenHash.set(tokenHash, normalized);
  }

  return {
    configPath,
    server: {
      host: root.server.host,
      port: root.server.port,
      adminTokenEnv: root.server.admin_token_env,
      adminToken,
      adminTokenHash: hashToken(adminToken),
      statePath: resolve(dirname(configPath), root.server.state_path),
      approvalTimeoutMs: root.server.approval_timeout_ms,
    },
    sources: sourceList,
    sourceMap,
    agents: agentList,
    agentMap,
    agentByTokenHash,
  };
}