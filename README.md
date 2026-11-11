# mcp-db

一个常驻数据库服务器的 MCP 服务：统一接入多个 SQL 数据源，按 **agent × source** 发放写权限，`approval` 模式下每条写语句执行前都要你确认。

**产品边界：保证不了数据写对，只保证写不出灾难。** 只读由数据库只读账号兜底，结构破坏和批量误伤会被拦住；逻辑污染（`UPDATE users SET vip = 1` 这种语法正确、业务错误的写入）不处理。

## 工作方式

- 单实例、多数据源；服务只监听 `127.0.0.1`，本机通过 SSH 隧道接入。
- 每个 source 两套 DSN：`dsn_readonly` 配只读账号，`dsn_readwrite` 配读写账号。
- 每个 agent 对每个 source 单独配置模式：`readonly` / `readwrite` / `approval`。
- `approval`：写语句执行前确认。客户端支持 MCP elicitation 时弹窗；不支持时进入待批队列，由本机 CLI 批准。
- 连接池由模式决定；SQL 分类器只决定“要不要问”，判错只会失败或多问，不会越权。

## 快速开始

```bash
bun install
cp mcp-db.example.toml mcp-db.toml     # 改 DSN 和 agent 的 source 模式

export MCPDB_ADMIN_TOKEN='<admin-token>'
export MCPDB_TOKEN_GBRAIN='<agent-token>'

bun run src/index.ts serve --config mcp-db.toml
```

服务默认监听 `127.0.0.1:8787`。远程部署时在本机开隧道：

```bash
ssh -L 8787:127.0.0.1:8787 user@db-server
```

MCP 客户端配置：

```json
{
  "mcpServers": {
    "db": {
      "type": "http",
      "url": "http://127.0.0.1:8787/mcp",
      "headers": { "Authorization": "Bearer <agent-token>" }
    }
  }
}
```

## CLI

```bash
mcp-db serve [--config mcp-db.toml]   # 在服务器上启动服务
mcp-db approvals                      # 列出待批写请求
mcp-db approve <id>                   # 批准
mcp-db deny <id>                      # 拒绝
```

`approve` / `deny` 通过 SSH 隧道调用服务的管理接口，使用 `admin_token_env` 指向的 token；本机需要一份能读到 `[server]` 的配置。

## MCP 工具

| 工具 | 说明 |
| --- | --- |
| `list_sources` | 当前 agent 可访问的 source 和模式 |
| `list_tables` | 表/视图清单 |
| `describe_table` | 列、主键、索引、外键 |
| `explain_query` | 查询计划（mssql 驱动暂不支持） |
| `execute_sql` | 执行一条语句；写语句按模式处理 |

`execute_sql` 的参数用对应数据库的原生占位符：PostgreSQL `$1`、MySQL/SQLite `?`、SQL Server `@p1`；值放在 `params` 数组里。

## 安全要点

- token 只从环境变量读取，进程内只保存 SHA-256；配置里只写变量名。
- 服务拒绝绑定非回环地址，必须经 SSH 隧道访问。
- `readonly` 的写语句走只读池，由数据库账号拒绝。
- 审计写入 `state_path` 的 SQLite（agent、source、SQL、耗时、行数、决定、确认渠道、拒绝原因）。
- 每次 `execute_sql` 最多一条语句；写语句不自动重试。

## 已知限制

- 逻辑污染不处理；没有写前影响行数预览和回滚。
- SQLite 驱动不支持语句超时；MSSQL 驱动不支持 `explain_query`。
- 只做了 SQLite 的自动化集成测试；PostgreSQL / MySQL / SQL Server 需要各自的真实实例验证。

## 开发

```bash
bun run typecheck
bun test
```

基线见 [SPEC.md](SPEC.md)，操作流程见 [AGENTS.md](AGENTS.md)。