# 变更记录

## 2026-11-17

- T-07 固定 PostgreSQL 首版资源默认值与边界：每 profile 5 并发连接、30s 原生语句超时；SQL 与元数据共享额度，连接与初始化等待有界。
- 普通 token 工具调用默认 60 次/60s，跨会话共享、重启重置；建库配额仍持久化且独立。
- query profile 每次实际连接须通过 PostgreSQL 原生只读准入探测；查询与 explain 在显式只读事务内运行。取消写入保守保留未知，发送前资源拒绝记录未执行。

## 2026-11-16

- 核心服务实现基线从 Bun/TypeScript/ESM 改为 Rust；旧实现保留为 legacy v1、迁移源和兼容测试对象，不再作为正式开发入口。
- 新增 [docs/PRD.md](docs/PRD.md)，产品意图、目标用户、场景、首版范围、成功指标和发布门禁以 PRD 为准；`SPEC.md` 继续承载系统设计、契约与 AC。
- 首版目标用户收敛为个人开发者/单管理员；必须交付只读查询、受控写入和受控建库。
- 首版普通数据库能力仅支持 PostgreSQL；MySQL/MariaDB、SQLite、SQL Server 通过后续驱动扩展任务交付。
- 首版交付形态为 Docker 服务；MCP 端口默认只发布到宿主回环地址或受控网络。
- 首版管理入口为 CLI + Web 管理页；管理页只通过宿主回环端口和 CLI 生成的一次性登录令牌访问。跨主机远程管理、TLS 终止和受信反向代理延后，不进入首版发布门禁。
- 首版发布前必须完成 PostgreSQL 只读边界、审批、建库、重启、迁移和审计证据；未被首版覆盖的 AC2 检查点保持未验证。

## 2026-11-13

- 服务不再假定部署在数据库服务器或全年常驻：可在任何能访问目标数据库的受信位置按需或常驻运行；默认本机监听，远程部署必须有 TLS、认证和网络访问控制。
- 授权模型收敛为 `(token, source, MCP tool)` 精确 grants；原 `readonly/readwrite` 仅可映射为管理页能力快捷模板，`approval` 单独迁移为执行策略，不再存在运行时 source 模式。
- 固定双 DSN 改为不定数量的命名 connection profiles，按 `query`、`mutate`、`schema`、`provision` 用途路由；同一凭证可复用但会降低隔离，provision 必须与普通 SQL 隔离。
- 普通 SQL 拆为 `query_sql`、`mutate_sql`、`execute_ddl`，弃用通用 `execute_sql`；工具范围按方言 fail-closed 解析，拒绝跨类、多语句、解析失败和未知语句。
- 新增专用 `create_database` tool；不再另设“允许 agent 建库”开关。建库必须同时有目标 source 的 tool grant、启用的 provision 映射、配额预留和每次 admin 批准。
- 生产 source 的普通 SQL 只读；高危 SQL 即使普通策略为 direct 也必须 admin 审批。通用 SQL 禁止数据库、账号、角色与授权管理，`DROP DATABASE` 不向 agent 开放。
- 默认每 token × 父建库 source 最多 5 个活动库、每 60 秒最多成功创建 1 个；source 归档/恢复仅限 admin，归档不删除物理数据库或释放物理存储。
- 完整工作区/共享工作区、TTL、快照恢复、自动删库、跨驱动存储硬限额和生产写入暂缓；本节是目标基线，实际实现和证据以 `TASK.md` 为准。

## 2026-11-11

- 权限模型定为每 agent × source 的三种静态模式：`readonly`、`readwrite`、`approval`；不再使用限时授权、`session` 模式和主动 `revoke`。
- `approval` 按写语句逐条确认：优先 MCP elicitation，客户端不支持时退回 CLI 待批队列（经 SSH 隧道调用管理接口）。
- 服务部署在数据库服务器，只绑 `127.0.0.1`，由本机 SSH 隧道接入；单实例管理多个 source，单用户使用。
- MCP 工具固定为 `list_sources`、`list_tables`、`describe_table`、`explain_query`、`execute_sql`，不随权限变化发送 `tools/list_changed`。
- 连接池由模式决定；SQL 分类器只决定是否需要审批，误判只会失败或多问，不会越权。