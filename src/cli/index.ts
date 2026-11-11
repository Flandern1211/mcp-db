import { loadConfig } from "../config.ts";
import { Approvals } from "../core/approval.ts";
import { Audit } from "../core/audit.ts";
import { PoolRegistry } from "../core/pools.ts";
import { Store } from "../core/store.ts";
import { startHttpServer } from "../transport/http.ts";

const USAGE = `mcp-db <command> [--config path]

  serve                 start the MCP server
  approvals             list pending write approvals
  approve <id>          approve a pending write
  deny <id>             deny a pending write

Commands other than serve call the admin API on the server (through the SSH tunnel).`;

function parseArgs(argv: string[]): { configPath: string; args: string[] } {
  let configPath = "mcp-db.toml";
  const args: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--config") {
      const value = argv[++i];
      if (!value) throw new Error("--config requires a path");
      configPath = value;
      continue;
    }
    args.push(argv[i]!);
  }
  return { configPath, args };
}

function adminBaseUrl(host: string, port: number): string {
  const formatted = host.includes(":") ? `[${host}]` : host;
  return `http://${formatted}:${port}`;
}

async function adminCall(configPath: string, path: string, method: "GET" | "POST"): Promise<number> {
  const config = loadConfig(configPath, { requireAgentTokens: false });
  const response = await fetch(`${adminBaseUrl(config.server.host, config.server.port)}${path}`, {
    method,
    headers: { authorization: `Bearer ${config.server.adminToken}` },
  });
  const body = await response.text();
  try {
    console.log(JSON.stringify(JSON.parse(body), null, 2));
  } catch {
    console.log(body);
  }
  return response.ok ? 0 : 1;
}

async function serve(configPath: string): Promise<number> {
  const config = loadConfig(configPath, { requireAgentTokens: true });
  const store = new Store(config.server.statePath);
  const audit = new Audit(store);
  const approvals = new Approvals(store, config.server.approvalTimeoutMs);
  const pools = new PoolRegistry(config.sourceMap);
  const running = await startHttpServer({ config, pools, approvals, audit });

  console.log(`mcp-db listening on ${adminBaseUrl(config.server.host, config.server.port)}/mcp`);

  await new Promise<void>((resolve) => {
    process.once("SIGINT", resolve);
    process.once("SIGTERM", resolve);
  });

  await running.close();
  approvals.close();
  await pools.closeAll();
  store.close();
  return 0;
}

export async function runCli(argv: string[]): Promise<number> {
  const { configPath, args } = parseArgs(argv);
  const [command, ...rest] = args;

  switch (command) {
    case "serve":
      return serve(configPath);

    case "approvals":
      return adminCall(configPath, "/admin/approvals", "GET");

    case "approve":
    case "deny": {
      const id = Number(rest[0]);
      if (!Number.isInteger(id) || id <= 0) {
        console.error(`usage: mcp-db ${command} <id>`);
        return 1;
      }
      const action = command === "approve" ? "approve" : "deny";
      return adminCall(configPath, `/admin/approvals/${id}/${action}`, "POST");
    }

    case undefined:
    case "-h":
    case "--help":
    case "help":
      console.log(USAGE);
      return command === undefined ? 1 : 0;

    default:
      console.error(`unknown command "${command}"\n\n${USAGE}`);
      return 1;
  }
}