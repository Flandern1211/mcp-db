# mcp-db

给 AI agent 用的多数据源 SQL MCP 服务：一个常驻进程同时接入多个数据库，按 **令牌 × 数据源 × 工具** 精确授权，写入可以要求逐次审批。

它不是"把数据库账号交给模型"，而是把三件事拆开分别控制：

| 问题 | 由什么回答 |
| --- | --- |
| 能不能做 | 令牌在每个数据源上被授予了哪些工具 |
| 用什么身份做 | 每个工具固定绑定的最小权限连接配置 |
| 要不要人批准 | 普通写入、高危操作、建库各自适用的审批策略 |

agent 拿不到 DSN，也不能切换执行身份；生产数据源对 agent 只读；库内 `DROP`/`TRUNCATE`、无 `WHERE` 的 `UPDATE`/`DELETE` 和建库始终需要管理员批准。

- 传输：Streamable HTTP MCP（`/mcp`），Bearer token 认证
- 管理面：内嵌中文 SPA（`/admin`），只发布到宿主回环地址，用 CLI 生成的一次性令牌或永久管理员密钥登录
- 交付形态：单个 OCI 镜像，状态库与加密凭证库都放在挂载卷里

## 支持的数据源

| 驱动 | 状态 | 说明 |
| --- | --- | --- |
| PostgreSQL | 首版已支持 | 元数据、只读查询、数据写入、库内 DDL；建库走审批路径 |
| SQLite | 已实现，未随首版发布 | 文件只读打开 + `query_only`；不支持建库 |
| MySQL/MariaDB | 未支持 | 在状态层和执行层明确拒绝 |
| SQL Server | 未支持 | 在状态层和执行层明确拒绝 |

未支持的驱动不会退回通用 SQL，也不会被静默降级。

## 快速开始

以下步骤在一台空机器上实测通过（Docker Desktop / Linux amd64，镜像 `mcp-db:dev`）。

### 1. 准备主密钥

加密凭证库用一把 32 字节主密钥加密，密钥由你自己保管。写 64 位十六进制即可：

```bash
mkdir -p ~/mcp-db/keys
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > ~/mcp-db/keys/master.key
chmod 600 ~/mcp-db/keys/master.key
```

### 2. 启动服务

```bash
docker run -d --name mcp-db \
  -v mcpdb-data:/data \
  -v "$HOME/mcp-db/keys:/keys:ro" \
  -e MCPDB_CREDENTIAL_STORE=/data/credentials.enc \
  -e MCPDB_MASTER_KEY_FILE=/keys/master.key \
  -p 127.0.0.1:8787:8787 \
  -p 127.0.0.1:8788:8788 \
  mcp-db:dev
```

`/data` 保存状态库 `state.db` 和加密凭证库 `credentials.enc`。首次启动会自动创建空凭证库；主密钥文件缺失或格式不对时启动直接失败，不会降级成"没有凭证也能连库"。

两个端口都只发布到 `127.0.0.1`：MCP 给本机客户端用，管理面只给本机管理员用。远程部署请用 SSH 隧道，不要把管理端口暴露到非回环地址。

Windows 的 Git Bash 会把以 `/` 开头的参数改写成 Windows 路径，执行上面的命令前先 `export MSYS_NO_PATHCONV=1`，或改用 PowerShell 并把密钥目录写成绝对路径。

### 3. 登录管理台

```bash
docker exec mcp-db mcp-db admin-login-token   # 5 分钟内有效，只能使用一次
```

打开 `http://127.0.0.1:8788/admin`，粘贴令牌登录。需要长期免登录时用 `docker exec mcp-db mcp-db admin-key create laptop` 生成永久管理员密钥，可随时吊销。

### 4. 添加数据源与连接配置

在「数据源」页新增数据源（首版驱动固定为 PostgreSQL），然后在数据源详情里：

1. **添加连接配置**：填名称、用途（查询 / 写入 / 结构变更 / 建库）和数据库连接串。连接串提交后立即加密写入凭证库，页面永不回显；「测试连接」只执行只读查询。
2. **添加工具路由**：为这个连接配置勾选它负责的工具，例如 `query_ro` 绑定 `list_tables`、`describe_table`、`query_sql`。

查询用途请用数据库只读账号，写入用读写账号，建库用具备 `CREATEDB` 权限的管理账号。复用同一个 DSN 不会产生数据库级隔离。

### 5. 签发令牌并授权

在「访问令牌」页签发令牌，明文只显示一次。然后在「管理授权」里选择数据源、勾选工具并保存——按源分组、一次保存，不做全局工具白名单。

### 6. 接入 MCP 客户端

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

### 7. 验证

```bash
curl -s http://127.0.0.1:8787/healthz
# {"service":"mcp-db","status":"ok"}
```

让客户端调用 `list_sources`，再调用 `query_sql` 执行一条只读查询。没有授权的工具会返回 `TOOL_NOT_GRANTED`，而不是被静默忽略。

## MCP 工具

| 工具 | 作用 | 绑定配置 |
| --- | --- | --- |
| `list_sources` | 列出该令牌有授权的数据源 | 无数据库连接 |
| `list_tables` | 表/视图清单 | 查询 |
| `describe_table` | 列、主键、索引、外键 | 查询 |
| `explain_query` | 只读执行计划 | 查询 |
| `query_sql` | 只读 SQL | 查询 |
| `mutate_sql` | 数据变更 | 写入 |
| `execute_ddl` | 库内对象 DDL | 结构变更 |
| `create_database` | 受控建库 | 建库（始终管理员审批） |

每次调用都会重新校验当前数据源上的精确授权；工具出现在列表里不等于可用。单次调用最多一条语句，SQL 按驱动方言解析，解析失败或多语句一律拒绝。

## 审批策略

- 新增写入授权默认 `approval`；管理员可按数据源和工具改成 `direct`。
- 路由还能设置不可下调的最低策略，实际策略取授权、路由最低策略和内置强制规则中最严格的一个。
- `direct` 不跳过高危审批：库内 `DROP`/`ALTER`/`TRUNCATE`、无 `WHERE` 的 `UPDATE`/`DELETE`、以及建库，都必须由管理员在管理页或 CLI 决定。
- 批准绑定精确请求摘要，只能消费一次；等待期间撤销令牌、归档数据源、停用连接配置或修改路由/策略，都会让旧批准失效。
- 外部执行结果未知时保留未知状态，不自动重试。

## 配置参考

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `MCPDB_ADDR` | `127.0.0.1:8787` | MCP 监听地址；镜像内为 `0.0.0.0:8787` |
| `MCPDB_ADMIN_ADDR` | `127.0.0.1:8788` | 管理监听地址；镜像内为 `0.0.0.0:8788` |
| `MCPDB_STATE_PATH` | `.mcp-db/state.db` | 状态库路径；镜像内为 `/data/state.db` |
| `MCPDB_CREDENTIAL_STORE` | 无 | 加密凭证库路径，与主密钥必须成对设置 |
| `MCPDB_MASTER_KEY_FILE` | 无 | 32 字节主密钥文件（64 位十六进制或 base64） |
| `MCPDB_HEALTHCHECK_ADDR` | `127.0.0.1:8787` | `--healthcheck` 探测地址 |
| `MCPDB_TOKEN_REQUESTS_PER_MINUTE` | `60` | 每个令牌每分钟工具调用上限（1～100000） |
| `MCPDB_PROVISION_MAX_ACTIVE` | `5` | 每令牌 × 父数据源的活动建库数量上限 |
| `MCPDB_PROVISION_MIN_INTERVAL_MS` | `60000` | 建库最小间隔 |
| `MCPDB_PROVISION_RESULT_TOOLS` | 内置模板 | 新建库授予的工具集合，可收紧或扩展 |

不配置 `MCPDB_CREDENTIAL_STORE`/`MCPDB_MASTER_KEY_FILE` 时服务仍可启动，但无法保存或解析 DSN，管理页保存连接串会直接报错。

## CLI

```bash
mcp-db                        # 启动 MCP 服务与管理面
mcp-db --healthcheck          # 健康检查
mcp-db admin-login-token      # 一次性登录令牌
mcp-db admin-key create <label>
mcp-db sources | sources add <id> <production|nonproduction> [postgres|sqlite] | sources delete <id>
mcp-db profiles <source> | profiles add <source> <name> <query|mutate|schema|provision> <credential_ref>
mcp-db routes add <source> <tool> <profile_id> [direct|approval]
mcp-db tokens issue <agent> | tokens revoke <token_id>
mcp-db grants <token_id> | grants add <token_id> <source> <tool> [direct|approval]
mcp-db audit [limit] | approvals [limit] | approve <id> | deny <id>
mcp-db archive-source <id> | restore-source <id>
```

`mcp-db --help` 列出完整命令。CLI 与管理面写同一个状态库，所以容器内执行命令时要和 service 用同一个 `MCPDB_STATE_PATH`。

## 安全边界

- 令牌明文只在签发时返回一次，服务端只保存 SHA-256；DSN 加密保存，不进入日志、审计或页面。
- 管理接口拒绝非回环 Host、跨源请求和任何 Bearer 凭证；agent token 不能访问管理面。
- 每个工具走固定连接配置，找不到可用映射就拒绝，不会自动升级身份。
- 生产数据源禁止授予 `mutate_sql` 和 `execute_ddl`。
- 应用层 SQL 分类不是最终边界：数据库原生最小权限、只读账号和只读会话才是兜底。
- 归档只阻止访问并保留记录，不删除物理数据库；管理台的删除只清控制面记录和托管凭证，物理数据库仍由人经数据库运维流程删除。

## 已知限制

- 不承诺"写不出灾难"：业务语义错误的合法写入（例如 `UPDATE users SET vip = 1`）不会被拦截。
- 不支持多管理员、团队/租户、列级授权、数据脱敏或跨驱动统一配额。
- SQLite 已实现但未随首版发布；MySQL/MariaDB 与 SQL Server 未支持。
- 建库结果未知或部分成功时保留隔离预留，需要管理员人工核对，不自动清理。

## 开发

```bash
bash scripts/verify.sh fast     # fmt + clippy + 143 项常规测试（固定 Rust 1.89 容器）
bash scripts/verify.sh full     # 追加 release 构建与 docker build
bash scripts/verify.sh real     # 临时 PostgreSQL 上的真实集成测试
bash scripts/closeout.sh        # 只读收口检查
node --test tests/*.test.cjs    # 管理台前端回归
```

生产部署（GitHub Actions → GHCR → Linux 主机）见 [docs/deploy.md](docs/deploy.md)。

架构、契约和验收标准见 [SPEC.md](SPEC.md)；产品意图与用户场景见 [docs/PRD.md](docs/PRD.md)；任务状态与证据见 [TASK.md](TASK.md)；变更记录见 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

[MIT](LICENSE)
