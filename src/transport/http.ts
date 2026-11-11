import { randomUUID } from "node:crypto";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { hashToken, parseBearer, tokenHashEquals } from "../auth.ts";
import type { AgentConfig, AppConfig } from "../config.ts";
import type { Approvals } from "../core/approval.ts";
import type { Audit } from "../core/audit.ts";
import type { PoolRegistry } from "../core/pools.ts";
import { createMcpServer } from "../tools/index.ts";

export interface ServerDeps {
  config: AppConfig;
  pools: PoolRegistry;
  approvals: Approvals;
  audit: Audit;
}

export interface RunningServer {
  httpServer: Server;
  close(): Promise<void>;
}

interface Session {
  transport: StreamableHTTPServerTransport;
  server: McpServer;
  agentId: string;
}

const MAX_BODY_BYTES = 10 * 1024 * 1024;

class HttpError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const payload = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json; charset=utf-8" });
  res.end(payload);
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += buffer.length;
    if (size > MAX_BODY_BYTES) throw new HttpError(413, "request body too large");
    chunks.push(buffer);
  }
  if (size === 0) return undefined;
  const body = Buffer.concat(chunks).toString("utf8");
  try {
    return JSON.parse(body);
  } catch {
    throw new HttpError(400, "invalid JSON body");
  }
}

function headerValue(req: IncomingMessage, name: string): string | undefined {
  const value = req.headers[name];
  return Array.isArray(value) ? value[0] : value;
}

export async function startHttpServer(deps: ServerDeps): Promise<RunningServer> {
  const { config, pools, approvals, audit } = deps;
  const sessions = new Map<string, Session>();

  function authenticateAgent(req: IncomingMessage): AgentConfig | null {
    const token = parseBearer(req.headers.authorization);
    if (!token) return null;
    return config.agentByTokenHash.get(hashToken(token)) ?? null;
  }

  function isAdmin(req: IncomingMessage): boolean {
    const token = parseBearer(req.headers.authorization);
    return token !== null && tokenHashEquals(hashToken(token), config.server.adminTokenHash);
  }

  async function handleAdmin(req: IncomingMessage, res: ServerResponse, url: URL): Promise<void> {
    if (!isAdmin(req)) {
      sendJson(res, 401, { error: "unauthorized" });
      return;
    }

    if (req.method === "GET" && url.pathname === "/admin/approvals") {
      sendJson(res, 200, { approvals: approvals.listPending() });
      return;
    }

    const match = /^\/admin\/approvals\/(\d+)\/(approve|deny)$/.exec(url.pathname);
    if (req.method === "POST" && match) {
      const id = Number(match[1]);
      const approve = match[2] === "approve";
      const record = approvals.get(id);
      if (!record) {
        sendJson(res, 404, { error: "approval not found" });
        return;
      }
      const status = approvals.decide(id, approve);
      if (status === "missing") {
        sendJson(res, 409, { error: "approval is no longer pending" });
        return;
      }
      audit.record({
        agent: "admin",
        source: record.source,
        tool: record.tool,
        sql: record.sql,
        paramsJson: record.paramsJson,
        decision: approve ? "allowed" : "rejected",
        channel: "cli",
        reason: `approval ${id} for agent ${record.agent}`,
      });
      sendJson(res, 200, { id, status });
      return;
    }

    sendJson(res, 404, { error: "not found" });
  }

  async function createSession(agent: AgentConfig): Promise<Session> {
    const state: { session?: Session } = {};
    const server = createMcpServer({ config, agent, pools, approvals, audit });
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: () => randomUUID(),
      onsessioninitialized: (sessionId) => {
        if (state.session) sessions.set(sessionId, state.session);
      },
    });
    transport.onclose = () => {
      const sessionId = transport.sessionId;
      if (sessionId) sessions.delete(sessionId);
      void server.close();
    };
    await server.connect(transport);
    state.session = { transport, server, agentId: agent.id };
    return state.session;
  }

  async function handleMcp(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const agent = authenticateAgent(req);
    if (!agent) {
      sendJson(res, 401, { error: "unauthorized" });
      return;
    }

    const sessionId = headerValue(req, "mcp-session-id");

    if (req.method === "POST") {
      const body = await readJson(req);
      if (sessionId) {
        const session = sessions.get(sessionId);
        if (!session) {
          sendJson(res, 404, { error: "session not found" });
          return;
        }
        if (session.agentId !== agent.id) {
          sendJson(res, 403, { error: "session belongs to a different agent" });
          return;
        }
        await session.transport.handleRequest(req, res, body);
        return;
      }

      const method = (body as { method?: unknown } | undefined)?.method;
      if (method !== "initialize") {
        sendJson(res, 400, { error: "initialize request required" });
        return;
      }

      const session = await createSession(agent);
      await session.transport.handleRequest(req, res, body);
      return;
    }

    if (req.method === "GET" || req.method === "DELETE") {
      if (!sessionId) {
        sendJson(res, 400, { error: "mcp-session-id header required" });
        return;
      }
      const session = sessions.get(sessionId);
      if (!session) {
        sendJson(res, 404, { error: "session not found" });
        return;
      }
      if (session.agentId !== agent.id) {
        sendJson(res, 403, { error: "session belongs to a different agent" });
        return;
      }
      await session.transport.handleRequest(req, res);
      return;
    }

    sendJson(res, 405, { error: "method not allowed" });
  }

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (url.pathname === "/mcp") {
      await handleMcp(req, res);
      return;
    }
    if (url.pathname.startsWith("/admin/")) {
      await handleAdmin(req, res, url);
      return;
    }
    sendJson(res, 404, { error: "not found" });
  }

  const httpServer = createServer((req, res) => {
    void handle(req, res).catch((err: unknown) => {
      if (res.headersSent) {
        res.end();
        return;
      }
      const status = err instanceof HttpError ? err.status : 500;
      const message = err instanceof Error ? err.message : String(err);
      sendJson(res, status, { error: message });
    });
  });

  await new Promise<void>((resolve, reject) => {
    httpServer.once("error", reject);
    httpServer.listen(config.server.port, config.server.host, () => resolve());
  });

  return {
    httpServer,
    async close() {
      for (const session of sessions.values()) {
        await session.transport.close().catch(() => undefined);
      }
      sessions.clear();
      httpServer.closeAllConnections();
      await new Promise<void>((resolve) => httpServer.close(() => resolve()));
    },
  };
}