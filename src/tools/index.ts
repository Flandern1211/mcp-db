import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import type { AgentConfig, AppConfig, SourceConfig, WriteMode } from "../config.ts";
import type { Approvals, Decision } from "../core/approval.ts";
import type { Audit } from "../core/audit.ts";
import type { PoolRegistry } from "../core/pools.ts";
import { classify } from "../core/sql.ts";
import type { PoolRole } from "../drivers/types.ts";
import { errorMessage } from "../drivers/types.ts";

export interface ToolContext {
  config: AppConfig;
  agent: AgentConfig;
  pools: PoolRegistry;
  approvals: Approvals;
  audit: Audit;
}

const MAX_MESSAGE_SQL = 4000;
const MAX_MESSAGE_PARAMS = 2000;

function text(data: unknown) {
  return { content: [{ type: "text" as const, text: JSON.stringify(data, null, 2) }] };
}

function truncate(value: string, limit: number): string {
  return value.length > limit ? `${value.slice(0, limit)}\n… (${value.length} chars total)` : value;
}

function approvalMessage(request: {
  agent: string;
  source: string;
  tool: string;
  sql: string;
  paramsJson: string | null;
}): string {
  const params = request.paramsJson ? `\nparams: ${truncate(request.paramsJson, MAX_MESSAGE_PARAMS)}` : "";
  return [
    `agent "${request.agent}" wants to run a write on source "${request.source}".`,
    "",
    truncate(request.sql, MAX_MESSAGE_SQL),
    params,
    "",
    "Approve this statement?",
  ].join("\n");
}

/**
 * Prefer the in-session elicitation dialog. If the client did not declare the
 * capability, fall back to the CLI queue and block until an operator decides.
 */
async function requireApproval(
  server: McpServer,
  ctx: ToolContext,
  request: { agent: string; source: string; tool: string; sql: string; paramsJson: string | null },
): Promise<Decision> {
  const capabilities = server.server.getClientCapabilities();
  if (capabilities?.elicitation) {
    try {
      const result = await server.server.elicitInput({
        message: approvalMessage(request),
        requestedSchema: {
          type: "object" as const,
          properties: {
            approve: {
              type: "boolean" as const,
              title: "执行这条写语句？",
              description: "拒绝或取消都不会执行",
              default: false,
            },
          },
          required: ["approve"],
        },
      });
      const approved = result.action === "accept" && result.content?.approve === true;
      return ctx.approvals.recordElicitation(request, approved);
    } catch (err) {
      return ctx.approvals.recordElicitation(request, false, `elicitation failed: ${errorMessage(err)}`);
    }
  }
  return ctx.approvals.requestViaCli(request);
}

/** One McpServer per MCP session, with the agent's source enum baked into the schemas. */
export function createMcpServer(ctx: ToolContext): McpServer {
  const server = new McpServer({ name: "mcp-db", version: "0.1.0" });
  const sourceIds = [...ctx.agent.sources.keys()];

  function resolveSource(sourceId?: string): { source: SourceConfig; mode: WriteMode } {
    const id = sourceId ?? (sourceIds.length === 1 ? sourceIds[0] : undefined);
    if (!id) {
      throw new Error(`source is required when the agent has multiple sources: ${sourceIds.join(", ")}`);
    }
    const mode = ctx.agent.sources.get(id);
    if (!mode) throw new Error(`source "${id}" is not available to this agent`);
    const source = ctx.config.sourceMap.get(id);
    if (!source) throw new Error(`unknown source "${id}"`);
    return { source, mode };
  }

  function defaultRole(mode: WriteMode): PoolRole {
    return mode === "readwrite" ? "rw" : "ro";
  }

  server.registerTool(
    "list_sources",
    { description: "List the database sources this agent can access." },
    async () =>
      text({
        sources: sourceIds.map((id) => {
          const source = ctx.config.sourceMap.get(id)!;
          return { id, label: source.label, driver: source.driver, mode: ctx.agent.sources.get(id) };
        }),
      }),
  );

  if (sourceIds.length === 0) return server;

  const SourceSchema = z.enum(sourceIds as [string, ...string[]]);
  const sourceField = {
    source: SourceSchema.optional().describe("Source id; may be omitted when the agent has one source"),
  };

  server.registerTool(
    "list_tables",
    {
      description: "List tables and views in a source.",
      inputSchema: { ...sourceField, schema: z.string().optional() },
    },
    async (args) => {
      const { source, mode } = resolveSource(args.source);
      const driver = await ctx.pools.get(source.id, defaultRole(mode));
      return text({ source: source.id, tables: await driver.listTables(args.schema) });
    },
  );

  server.registerTool(
    "describe_table",
    {
      description: "Describe a table: columns, primary keys, indexes and foreign keys.",
      inputSchema: { ...sourceField, table: z.string(), schema: z.string().optional() },
    },
    async (args) => {
      const { source, mode } = resolveSource(args.source);
      const driver = await ctx.pools.get(source.id, defaultRole(mode));
      return text({ source: source.id, ...(await driver.describeTable(args.table, args.schema)) });
    },
  );

  server.registerTool(
    "explain_query",
    {
      description: "Return the database query plan for a statement.",
      inputSchema: {
        ...sourceField,
        sql: z.string(),
        params: z.array(z.unknown()).optional(),
      },
    },
    async (args) => {
      const { source, mode } = resolveSource(args.source);
      const driver = await ctx.pools.get(source.id, defaultRole(mode));
      return text({ source: source.id, plan: await driver.explain(args.sql, args.params ?? []) });
    },
  );

  server.registerTool(
    "execute_sql",
    {
      description:
        "Execute one SQL statement. In approval mode every write asks the user first. Use native placeholders and pass values in params.",
      inputSchema: {
        ...sourceField,
        sql: z.string(),
        params: z.array(z.unknown()).optional(),
      },
    },
    async (args) => {
      const { source, mode } = resolveSource(args.source);
      const params = args.params ?? [];
      // Pending rows and audit only need enough to identify the statement.
      const paramsJson = JSON.stringify(params).slice(0, MAX_MESSAGE_PARAMS);
      const kind = classify(args.sql);
      const started = performance.now();

      let role = defaultRole(mode);
      let channel: string | null = null;

      if (mode === "approval" && kind === "write") {
        const decision = await requireApproval(server, ctx, {
          agent: ctx.agent.id,
          source: source.id,
          tool: "execute_sql",
          sql: args.sql,
          paramsJson,
        });
        if (!decision.approved) {
          ctx.audit.record({
            agent: ctx.agent.id,
            source: source.id,
            tool: "execute_sql",
            sql: args.sql,
            paramsJson,
            decision: "rejected",
            channel: decision.channel,
            reason: decision.reason,
          });
          throw new Error(`write rejected (${decision.channel}): ${decision.reason ?? "not approved"}`);
        }
        channel = decision.channel;
        role = "rw";
      }

      try {
        const driver = await ctx.pools.get(source.id, role);
        const result = await driver.execute(args.sql, params);
        const truncated = result.rows.length > source.maxRows;
        const rows = truncated ? result.rows.slice(0, source.maxRows) : result.rows;

        ctx.audit.record({
          agent: ctx.agent.id,
          source: source.id,
          tool: "execute_sql",
          sql: args.sql,
          paramsJson,
          durationMs: result.durationMs,
          rowCount: result.rowCount,
          decision: "allowed",
          channel,
        });

        return text({
          source: source.id,
          mode,
          columns: result.columns,
          rows,
          rowCount: result.rowCount,
          returnedRows: rows.length,
          truncated,
          durationMs: result.durationMs,
        });
      } catch (err) {
        ctx.audit.record({
          agent: ctx.agent.id,
          source: source.id,
          tool: "execute_sql",
          sql: args.sql,
          paramsJson,
          durationMs: Math.round(performance.now() - started),
          decision: "error",
          channel,
          reason: errorMessage(err),
        });
        throw err;
      }
    },
  );

  return server;
}