# mcp-db 规格

- 状态：当前基线
- 更新时间：2026-11-11
- 变更记录：[CHANGELOG.md](CHANGELOG.md)

## 1. 范围与非目标

mcp-db 是一个常驻数据库服务器的 HTTP MCP 服务：统一接入多个 SQL 数据源，按 agent 发放静态读写权限；需要写入时，`approval` 模式逐条向用户确认。产品边界是**保证不了数据写对，只保证写不出灾难**。

### 1.1 范围

v1 交付：

- 单实例、多数据源的 Streamable HTTP MCP 服务；服务只监听 `127.0.0.1`，本机客户端通过 SSH 隧道接入。
- 驱动：PostgreSQL、MySQL/MariaDB、SQLite、SQL Server。
- 每个 source 两套 DSN（只读、读写）与两个连接池；权限模式决定使用哪个池。
- agent 身份来自 `Authorization: Bearer <token>`；token 只从环境变量读取，进程内只保存哈希。
- 权限按 **agent × source** 配置，固定三档：
  - `readonly`：只读，所有语句走只读池；
  - `readwrite`：读写，所有语句走读写池；
  - `approval`：默认只读；每条写语句执行前向用户确认，确认后该条语句走读写池。
- 确认通道：优先 MCP elicitation；客户端未声明该能力时，进入待批队列，由本机 CLI 经 SSH 隧道调用管理接口批准或拒绝。
- 执行保护：`params` 参数绑定、`max_rows` 结果截断、语句超时、审计。
- 单用户使用，不区分角色、审批人或归属。

### 1.2 非目标

- 不解决逻辑污染；不做写前影响行数预览、回滚或 undo。
- 不做限时写授权、`session` 模式、常驻授权状态或主动 `revoke`；写权限只有上述静态三档。
- 不做工具列表随权限变化（模式静态，工具集在会话建立时固定）。
- 不支持 NoSQL；Oracle 只在驱动接口上留位置。
- 不做 stdio transport、多用户/团队、Web 管理页、IM 审批。
- 不替代数据库自身的备份、账号权限和审计体系。

## 2. 需求与验收

| 编号 | 需求 | 验收标准 |
| --- | --- | --- |
| `AC-01` | agent 身份与 source 可见性 | 缺少或未知 token 时 `/mcp` 返回 401；`list_sources` 只返回该 agent 配置的 source；`execute_sql` 的 `source` 参数只接受该 agent 可访问的 source，其他值返回参数错误。 |
| `AC-02` | `readonly` 无法写入 | `readonly` 的 agent 执行写语句时走只读池，由数据库拒绝并返回错误；库内数据与 schema 不变。 |
| `AC-03` | `readwrite` 直接读写 | `readwrite` 的 agent 读、写语句都走读写池；写语句执行成功并落库。 |
| `AC-04` | `approval` + elicitation 逐条确认 | 客户端声明 `elicitation` 能力时，每条写语句触发一次确认，消息包含 agent、source、SQL；用户 accept 后该语句执行，decline/cancel 后不执行；读语句不触发确认。 |
| `AC-05` | `approval` + CLI 兜底 | 客户端未声明 `elicitation` 能力时，写语句进入待批队列；`mcp-db approvals` 可见该请求；`mcp-db approve <id>` 后语句执行，`deny` 或等待超时后返回拒绝且不执行。 |
| `AC-06` | 参数绑定 | `execute_sql` 只通过 `params` 传参；参数中的引号、分号不改变语句结构。 |
| `AC-07` | 结果集截断 | 结果行数超过 source 的 `max_rows` 时最多返回 `max_rows` 行，并显式标记结果被截断。 |
| `AC-08` | 语句超时 | 超过 `statement_timeout_ms` 的语句返回超时错误，连接池不被挂死，后续请求可正常执行。 |
| `AC-09` | 审计 | 每次 `execute_sql` 写一条审计（agent、source、SQL、参数摘要、耗时、行数、决定、确认渠道、拒绝原因）；`approve`/`deny` 决策另记审计。 |
| `AC-10` | 管理面隔离 | `/admin/*` 只接受 admin token；agent token 访问返回 401/403；服务只监听 `127.0.0.1`，不直接暴露公网或内网地址。 |
| `AC-11` | 配置校验 | 非法 DSN、未知 source、非法模式、审批超时非正数时启动失败，并指出出错的字段路径。 |

需要端到端验证的 AC 及检查点：

- `AC-04`：建立 MCP 会话 → 写语句触发 `elicitation/create` → 客户端返回 accept → 语句实际执行并落库；另跑一次 decline，确认未执行。
- `AC-05`：使用未声明 elicitation 的客户端 → 写语句进入待批队列且调用挂起 → 本机 CLI 经隧道批准 → 语句执行；另跑一次拒绝/超时，确认返回错误且未执行。
- `AC-09`：执行一次读、一次写、一次拒绝 → 状态库存在三条审计且字段与实际调用一致。

## 3. 架构边界与关键决策

- 系统边界：内部组件是配置、HTTP transport、身份鉴权、工具层、连接池注册表、方言驱动、审批（elicitation 与 CLI 队列）、审计/状态存储、管理接口和 CLI；外部依赖是 MCP 客户端、被访问的数据库、环境变量中的 token。
- 部署与运行形态：单进程常驻数据库服务器，Bun 直接运行 TypeScript；MCP 端点 `/mcp`，管理端点 `/admin/*`；只绑 `127.0.0.1:8787`，本机通过 `ssh -L 8787:127.0.0.1:8787` 接入。状态库默认 `./.mcp-db/state.db`。
- 工程基线：Bun 1.4.4、TypeScript 5.6+、ESM、`bun.lock`；v1 只支持单机部署。

组件与职责（模块边界；实现进度以代码和 `TASK.md` 为准）：

| 组件 | 职责 |
| --- | --- |
| `src/config.ts` | 解析并校验 `mcp-db.toml`；token 只从环境变量读取并哈希 |
| `src/auth.ts` | Bearer token 解析、agent 与 admin 身份区分 |
| `src/transport/http.ts` | Streamable HTTP 接入、MCP 会话、能力协商、管理路由挂载 |
| `src/tools/*.ts` | MCP 工具定义与 zod schema |
| `src/core/pools.ts` | 按 source × role 建池，按模式选择池 |
| `src/drivers/*.ts` | 各方言元数据查询与执行适配 |
| `src/core/sql.ts` | 语句读/写分类，只用于判断是否需要审批 |
| `src/core/approval.ts` | elicitation 确认与 CLI 待批队列 |
| `src/core/store.ts` | 待批请求与审计的本地持久化 |
| `src/core/audit.ts` | 审计记录写入，失败不影响请求 |
| `src/admin/*` | 管理接口：待批列表、批准、拒绝 |
| `src/cli/*` | 本机 CLI：serve、approvals、approve、deny |
| `src/index.ts` | 进程入口与子命令分发 |

关键取舍：

1. **HTTP 而非 stdio**：服务在数据库服务器，客户端在本机；HTTP 是跨机器接入的直接方式，stdio 只能同机。
2. **双 DSN、双连接池**：只读由数据库账号兜底；SQL 分类器可被绕过，不能当安全边界。
3. **池由模式决定，分类器只决定是否审批**：分类器把写判成读，语句仍会打到只读池并失败；把读判成写，只会多问一次。两种误判都不会越权。
4. **逐条确认而非限时授权**：批准只对当前语句生效，没有需要过期或收回的后台状态。
5. **静态三档、不在运行期变权限**：因此不使用 `notifications/tools/list_changed`，工具集按 agent 在会话建立时确定。
6. **单实例多 source**：一份配置、一套审计、一套待批队列。

## 4. 对外契约与数据模型

- MCP 工具契约：机器可读形状以 `src/tools/` 中的 zod schema 为准（尚未实现）；若引入独立 OpenAPI/JSON Schema 契约文件，在此链接，本文只保留非机器可读约定。
- MCP 工具：

| 工具 | 可见性 | 说明 |
| --- | --- | --- |
| `list_sources` | 总是 | 该 agent 可访问的 source，不返回 DSN |
| `list_tables` | 总是 | 表/视图清单；`source` 只能是该 agent 可访问的 source |
| `describe_table` | 总是 | 列、类型、可空、默认值、主键、索引、外键 |
| `explain_query` | 总是 | 查询计划；`EXPLAIN ANALYZE` 会实际执行语句，按写语句处理 |
| `execute_sql` | 总是 | `sql`、`params`、`source`；行为由该 source 的模式决定 |

- 管理接口（仅 `127.0.0.1`，`Authorization: Bearer <admin_token>`）：
  - `GET /admin/approvals`：待批请求列表；
  - `POST /admin/approvals/{id}/approve`：批准，挂起的工具调用继续执行；
  - `POST /admin/approvals/{id}/deny`：拒绝，挂起的调用返回错误。
- CLI（本机，经隧道访问管理接口）：`mcp-db approvals`、`mcp-db approve <id>`、`mcp-db deny <id>`；`mcp-db serve` 在服务器上启动服务。
- 配置形状（完整示例随实现提供）：

```toml
[server]
host = "127.0.0.1"
port = 8787
admin_token_env = "MCPDB_ADMIN_TOKEN"
state_path = "./.mcp-db/state.db"
approval_timeout_ms = 120000

[[sources]]
id = "production"
driver = "postgres"
dsn_readonly = "postgres://ro_user:<redacted>@127.0.0.1:5432/app"
dsn_readwrite = "postgres://rw_user:<redacted>@127.0.0.1:5432/app"
max_rows = 500
statement_timeout_ms = 15000

[agents.gbrain]
token_env = "MCPDB_TOKEN_GBRAIN"
sources = { production = "approval" }
```

- 错误语义：未知/缺失 token → 401；source 不在该 agent 的配置内 → 参数错误；`approval` 被拒绝或超时 → 工具返回拒绝错误；语句超时 → 超时错误；配置非法 → 启动失败并指出字段路径。
- 数据模型（`state_path` 的 SQLite，v1 拟定）：

| 表 | 关键字段 |
| --- | --- |
| `approvals` | `id`、`agent`、`source`、`tool`、`sql`、`params_json`、`status`（pending/approved/denied/expired）、`channel`（elicitation/cli）、`created_at`、`expires_at`、`decided_at` |
| `audit` | `id`、`ts`、`agent`、`source`、`tool`、`sql`、`duration_ms`、`row_count`、`decision`（allowed/rejected/error）、`channel`、`reason` |

- 外部写：只有 `execute_sql` 的写语句会写业务库，每条语句最多执行一次；审批与审计只写本地状态库。
- 敏感数据：token 只存 SHA-256；DSN 在 `mcp-db.toml` 中明文保存但不提交；审计保存 SQL 与参数摘要，可能含业务数据，按本机敏感材料处理。

## 5. 未决问题

| 问题 | 影响 | 负责人 | 截止时间 |
| --- | --- | --- | --- |
| 目标客户端（Alma）是否支持 elicitation | 决定 approval 走弹窗还是全程 CLI 兜底；两条路径都要实现 | 项目所有者 | 实现审批前实测 |
| `approval_timeout_ms` 默认值与客户端工具超时的配合 | 超时过短会误拒，过长会挂住 agent | 项目所有者 | 实现审批前 |
| 长 SQL 在 elicitation 消息中的截断展示规则 | 影响用户能否看清要执行的语句 | 项目所有者 | 实现审批前 |
| CLI 的分发形式（`bun build` 单文件或脚本） | 影响本机安装和使用方式 | 项目所有者 | 发布 v1 前 |
| `sources test` 是否需要，以及由服务端还是 CLI 直接连接 | 影响自检入口和配置校验范围 | 项目所有者 | 实现 CLI 时 |

## 6. 维护规则

1. 只在用户可见行为、对外契约、数据模型、架构边界变化时更新本文件，并在 `CHANGELOG.md` 记一条。
2. 任务级验收微调写任务或提交，不上升为基线。
3. 超过约 300 行时按领域拆分到 `spec/<domain>.md`，保留一个索引。
4. 同一主题只保留一个权威正文；历史材料不进入本文件。
5. 产品意图、目标用户、场景和范围变化时更新 `docs/PRD.md`；SPEC 不重复产品叙事。
6. 对外 API 的机器可读契约以链接的 OpenAPI/schema 文件为准；本文件只保留链接、适用范围和非机器可读约定。