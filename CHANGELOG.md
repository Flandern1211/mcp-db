# 变更记录

## 2026-11-25

- 数据源新增「删除」（AC2-21 基线修订）：在归档之外提供物理删除控制面记录，可随时对活动或已归档 source 执行且不可撤销。同一事务内删除 source 及其 profiles、routes、grants 和已结算建库预留；`audit_events` 与审批历史保留，物理数据库永不删除，ID 删除后可复用。存在待批/已批准审批、子 source 或未结算建库预留时拒绝删除并提示先处理，不做强制级联。管理页列表与详情页提供删除入口（须输入数据源 ID 确认），删除后同时清理无其他 profile 引用的托管凭证；CLI 新增 `mcp-db sources delete <id>`，只清控制面记录并逐条提示保留的凭证引用，因为运行中的服务是凭证库唯一写者。

## 2026-11-24

- 管理面新增可选公网 HTTPS 入口（AC2-10 基线修订）：受信反向代理终止 TLS 后转发到仅回环发布的管理端口，应用按 `X-Forwarded-Proto`/`X-Forwarded-Host` 校验 Host 与 Origin，HTTPS 下 `mcpdb_admin` cookie 附加 Secure；未配置代理头时保持回环限制，直接访问非回环 Host 仍被拒绝。部署单元新增 Caddy 服务（443 → /admin、/mcp），主机无域名时使用内部自签证书。


## 2026-11-23

- 新增 GitHub Actions 交付流水线：PR 与推送执行脚本回归、fmt、clippy、locked 测试、镜像构建和容器冒烟；`main` 推送把冒烟通过的镜像以不可变摘要发布到 GHCR，每次部署从该提交同步 `deploy/` 到目标主机再按摘要滚动。部署单元只把 MCP 与管理端口发布到宿主回环地址，运维手册见 [docs/deploy.md](docs/deploy.md)。
- 服务启动时若配置了 `MCPDB_CREDENTIAL_STORE` 与 `MCPDB_MASTER_KEY_FILE` 而凭证库文件尚不存在，现在会创建空凭证库，使全新部署可以直接在管理页录入连接串；主密钥缺失或非法仍在启动阶段失败。此前只有 legacy 迁移会创建该文件，干净环境无法保存 DSN。
- README 重写为面向使用者的入口：能力与边界、快速开始、MCP 客户端接入、工具与审批策略、配置参考、CLI、安全边界与已知限制；移除只适用于 Bun/TypeScript v1 的历史说明。

## 2026-11-20

- T-17 管理台按数据源汇总和编辑令牌授权，保留同名 Agent 各自的令牌边界；按源原子保存、拒绝过期快照、保留未改策略，并失效受撤销或策略变更影响的未消费审批。不改 MCP 工具契约或存储模型。

## 2026-11-19

- T-13 管理台改为内嵌中文 SPA（概览 / 访问令牌 / 待批请求 / 审计日志 / 数据源），同源 `/admin/api/*` 提供 source、profile、route、token、grant、approval、audit 的读写接口；不上 CDN、不使用前端框架。
- 连接配置与工具路由支持管理页新增、修改、删除；用途与 tool 不可变，修改递增 `version`，旧审批执行前因版本变化被拒绝；仍被路由或未完成审批引用的对象不允许删除。
- 管理页可直接录入 PostgreSQL 连接串，服务端立即加密写入凭证库，页面只显示“已加密配置”；“测试连接”仅执行只读查询并返回数据库、账号、服务端版本和耗时。
- 新增永久管理员密钥：`admin-key create/list/revoke`，只存 SHA-256 hash、可随时吊销并记录 `last_used_at`；登录同时接受一次性令牌或管理员密钥，agent Bearer 仍不能访问管理面。
- 基线同步更新 `SPEC.md` 与 `docs/PRD.md` 的管理面、AC2-10 和 profile/route 管理约定。
- T-16 管理页令牌授权按工具路由置灰：无已启用 route 的工具不可选并标注「未配置工具路由」或「连接配置已停用」，全部不可用时禁止提交；工具仍全量展示，生产库写工具仍隐藏。

## 2026-11-18

- T-03 legacy v1 迁移：`migrate-legacy` 先快照再 staging 导入，通过持久 journal 完成状态库与 TOML 的 cutover；`recover-legacy-migration` 只能前滚已提交切换或回滚提交点前状态。
- 旧 `readonly`/`readwrite`/`approval` 按固定矩阵转换为 grants 与策略，从不生成 `create_database`；旧 source 迁移后保持 archived，需 `sources set-environment` 分类后由 admin 恢复。
- 旧 DSN 移入加密凭证库，活动 TOML 只保留 server 段，旧配置备份 0600 保护；存在未 checkpoint WAL 或 production 与写模式冲突时在写入前拒绝迁移。
- 生产 source 存在写 grants 时拒绝 `set-environment production`，与 AC2-14 保持一致。

## 2026-11-17

- T-07 固定 PostgreSQL 首版资源默认值与边界：每 profile 5 并发连接、30s 原生语句超时；SQL 与元数据共享额度，连接与初始化等待有界。
- 普通 token 工具调用默认 60 次/60s，跨会话共享、重启重置；建库配额仍持久化且独立。
- query profile 每次实际连接须通过 PostgreSQL 原生只读准入探测；查询与 explain 在显式只读事务内运行。取消写入保守保留未知，发送前资源拒绝记录未执行。
- T-08 建库闭环：服务端生成库名与 `_ro`/`_rw` 身份并由 provision 账号保留库所有权；新库 DSN 写入加密凭证库后热加载，provision 管理凭证限定在 `keyring:provision/` 命名空间，普通 profile 不得引用。
- 建库配额默认每 token × 父 source 5 个活动库、60 秒最多成功 1 个，可用 `MCPDB_PROVISION_*` 收紧；审批前检查，执行前原子预留；未知/部分成功隔离并阻塞后续创建，admin 结算后恢复，重启把执行中预留转为隔离。
- 结果 grants 按管理员模板生成，生产新库强制只读且永不含 `create_database`；归档/恢复与审批通过本机 CLI 操作，归档不删除物理数据库。
- T-09 本机管理面：新增独立管理监听 `MCPDB_ADMIN_ADDR`（默认 `127.0.0.1:8788`），Web 提供登录、审批、审计、归档/恢复与隔离预留结算；Docker 必须把管理端口发布到宿主回环地址，admin 与 MCP 端口分离。
- Web 登录使用 CLI 生成的 5 分钟一次性令牌（只存 SHA-256 hash），会话 1 小时且重启失效；cookie HttpOnly + SameSite=Strict，写请求校验 CSRF 与 Origin，Host 必须回环，agent Bearer 不能访问管理面。
- CLI 补齐 source/profile/route/token/grant 管理与审计查询；每次管理变更写入 `admin_*` 审计，不含令牌明文、hash、DSN 或口令。

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
