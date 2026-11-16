# mcp-db 产品需求文档

| 项目 | 内容 |
| --- | --- |
| 状态 | 当前产品基线 |
| 更新时间 | 2026-11-16 |
| 产品阶段 | 首版 Rust AC2，尚未实现 |
| 目标用户 | 个人开发者 / 单管理员 |
| 交付形态 | Docker 服务 |
| 权威边界 | 本文负责产品意图、用户、场景、范围与成功指标；系统设计、契约和 AC 见 [SPEC.md](../SPEC.md)；任务状态与证据见 [TASK.md](../TASK.md) |

## 1. 产品愿景

mcp-db 为 AI agent 提供一个统一、可审计、可精确授权的多 SQL 数据源 MCP 入口。

管理员可以让不同 agent 只看到并获得自己需要的数据库能力，而不是把一套数据库账号和任意 SQL 执行权交给所有客户端。产品通过三层分离控制风险：

1. **能不能**：token 在每个 source 上获授哪些 tools。
2. **用什么身份**：每个 tool 固定使用哪个最小权限连接 profile。
3. **要不要批准**：普通写入、高危操作和建库分别适用什么审批策略。

## 2. 目标用户与角色

### 2.1 主要用户

个人开发者或单人维护者，在自己控制的电脑、开发机或内网环境中运行 mcp-db，并为一个或多个 AI agent 提供数据库访问。

典型特征：

- 同时管理少量数据库 source；
- 愿意为不同 agent 签发不同 token；
- 需要只读查询、偶发受控写入和受控建库；
- 能自行提供数据库账号、备份和物理删除能力；
- 不要求多人管理、组织、团队或租户体系。

### 2.2 参与角色

| 角色 | 职责 |
| --- | --- |
| 管理员 | 配置 source/profile、签发 token、设置 grants 与审批策略、处理审批、查看审计、归档/恢复 source |
| MCP agent/client | 使用 Bearer token 发现并调用获授权的数据库 tools |
| 数据库管理员/运维 | 在产品外提供数据库原生最小权限、备份恢复和物理删库 |

管理员是唯一的人类控制面角色。agent 不能管理 token、profile、route、审批或 source 生命周期。

## 3. 核心用户旅程

### 3.1 首次接入

1. 管理员通过 Docker 启动服务，仅将 MCP 端口发布到宿主回环地址或受控网络。
2. 管理员用 CLI 登记 PostgreSQL source 和最小权限连接 profiles。
3. 管理员签发 token，明文只展示一次。
4. 管理员按 source → tool 授予查询、写入或建库能力。
5. agent 建立 MCP 会话，发现可用 source 和 tools。
6. agent 执行第一次只读查询，并得到明确结果或可区分的拒绝原因。

### 3.2 日常只读

1. agent 发现 source 和元数据 tools。
2. agent 查询表、查看结构或执行只读 SQL。
3. 数据库原生只读身份拒绝任何绕过 MCP 分类的数据写入或结构变更。
4. 管理员可在审计中看到身份、source、tool、profile、决定和结果信息。

### 3.3 受控写入

1. agent 对非生产 source 发起数据写入或库内 DDL。
2. 服务按 grant、route 最低策略和内置高危规则取最严格策略。
3. 普通写入按配置直接执行、通过 MCP elicitation 确认，或进入管理员待批队列。
4. 高危操作始终由管理员在 Web 管理页或 CLI 决定。
5. 批准只对精确请求有效且只能消费一次；等待期间 token 撤销、source 归档或 route 变更会使批准失效。

### 3.4 受控建库

1. 只有同时具备 `create_database` grant 和启用 provision 映射的 token 才能申请。
2. agent 只提供用途等受限信息；数据库名称、主机、凭证和新 source 配置由服务端或管理员预设控制。
3. 管理员逐次批准，并看到创建目标、环境、profile 身份摘要和将授予创建者的新 source tools。
4. 配额在外部执行前原子预留；并发竞争失败不执行。
5. 新 source 只有在物理创建、profiles、grants 和本地登记全部就绪后才对创建 token 可见。
6. 新 source 不继承 provision 能力，其他 token 默认不可见。
7. 结果未知或部分成功时保留隔离状态，等待管理员核对，不自动重试或谎报成功。

### 3.5 管理与运维

1. 管理员通过 CLI 和本机 Web 管理页处理审批、查看审计、管理 token/profile/route。
2. 管理页只通过发布到宿主回环地址的独立端口访问，并要求 CLI 生成的一次性登录令牌。
3. 管理员可以归档 source；归档阻止新请求、使待批失效并关闭连接，但不删除物理数据库。
4. 管理员重启服务后，配置、grants、审计、配额和隔离预留仍然存在；旧批准和外部写不重放。

### 3.6 从 v1 迁移

1. 管理员停止 legacy Bun/TypeScript v1 服务。
2. 按迁移流程导入旧 TOML 与状态库。
3. 旧 source 默认保持禁用，直到管理员确认环境、只读边界和 grants。
4. 切换后活动配置不再包含旧凭证或旧权限，旧 binary 不能借此恢复 source 权限。
5. 新服务只注册 AC2 的 tools，旧 `execute_sql` 调用被拒绝。

## 4. 首版范围

首版是一个可发布的 Rust AC2 产品闭环，按以下边界交付。

### 4.1 必须交付

- Rust 单进程 HTTP MCP 服务，提供 OCI 容器镜像。
- Docker 部署；MCP 端口默认只发布到宿主回环地址或受控网络。
- PostgreSQL source 的只读查询、受控写入和库内 DDL。
- PostgreSQL 受控建库、配额、新 source 登记与归档/恢复。
- 精确 `(token, source, tool)` grants。
- 任意数量的命名 connection profiles，按 query/mutate/schema/provision 用途路由。
- `list_sources`、`list_tables`、`describe_table`、`explain_query`、`query_sql`、`mutate_sql`、`execute_ddl`、`create_database`。
- 普通写入 direct/approval 策略、elicitation、管理员待批队列和高危强制审批。
- token 签发/撤销、grant/profile/route 管理、审批和审计持久化。
- 本机 Web 管理页与 CLI 管理面；管理页使用一次性登录令牌。
- 参数绑定、显式结果截断、连接数、语句超时、请求速率和建库配额。
- 从 Bun/TypeScript v1 的可恢复迁移。
- Docker 健康检查、非 root 运行和最小镜像运行依赖。

### 4.2 后续阶段

- MySQL/MariaDB、SQLite、SQL Server 的普通数据库能力和只读边界验证。
- 远程管理入口、TLS 终止、受信反向代理、Origin/CSRF 的跨主机安全模型。
- 多管理员、组织、团队、租户、共享工作区。
- TTL、快照、一键恢复、自动删库、跨驱动存储硬限额。
- 数据脱敏、列级授权、RLS 管理。
- NoSQL、stdio transport、IM 审批、分布式多实例。
- 原生 Windows/macOS 安装包和自动更新。

### 4.3 非目标

- 不承诺拦截所有危险 SQL，也不保证业务语义正确。
- 不开放 agent 写生产数据。
- 不提供 agent 删库、账号/角色管理或权限管理。
- 不替代数据库备份、恢复、原生资源治理和审计。
- 不用结果截断冒充数据库扫描成本限制。
- 不用 source 归档冒充物理删库或释放存储。

## 5. 产品原则与安全承诺

1. **默认拒绝**：没有明确 grant 的 source/tool 一律拒绝。
2. **可见不等于可用**：工具出现在列表中仍必须在调用时复核精确 grant。
3. **身份不由 agent 选择**：agent 不能提供 DSN、切换 profile 或提升数据库身份。
4. **数据库权限是最终边界**：应用层 SQL 分类不能替代数据库原生最小权限。
5. **高危不可降级**：高危 SQL 和建库即使 grant 为 direct，也必须管理员审批。
6. **批准绑定精确请求**：参数、profile、route、策略或状态变化会使旧批准失效。
7. **未知结果不重试**：外部执行结果未知时保留状态并等待人工核对。
8. **迁移不混合授权**：任一时刻只允许一个授权事实源。
9. **管理面不对 agent 开放**：agent token 不能访问管理 API 或管理页。
10. **归档不等于删除**：归档只阻止访问并保留记录，不承诺释放物理存储。

## 6. 成功指标

首版不设采用率或性能数字目标，以可判定的产品结果作为成功标准：

| 指标 | 目标 |
| --- | --- |
| 未授权调用 | 未授权 source/tool 调用的成功次数为 0 |
| 只读边界 | 已发布驱动（首版 PostgreSQL）通过 query profile 绕过 MCP 分类直接写入数据或结构时，全部被数据库拒绝且夹具不变 |
| 审批 | 高危和建库请求均逐次由管理员决定；重复消费成功次数为 0 |
| 重启安全 | 服务重启后旧 pending/approved 请求自动执行次数为 0；未知外部写重试次数为 0 |
| 授权一致性 | cutover 后同时存在新旧权限的事实状态为 0 |
| 首次可用 | 从空 Docker 环境开始，按文档完成 source/profile 配置、token 签发、管理页登录和一次授权只读查询 |
| 可审计 | 每次拒绝、审批、执行和生命周期操作都能定位到身份、source、tool、决定和原因 |

## 7. 首发验收门禁

以下场景全部有独立证据后，首版才可标记可发布：

1. Docker 镜像可构建、以非 root 启动、健康检查通过。
2. CLI 可登记 PostgreSQL source/profile、签发和撤销 token、配置 grants 与 routes。
3. Web 管理页可用一次性登录令牌登录；agent token 无法访问管理面。
4. 只读 token 能完成发现和查询；直接构造未授权 source/tool 调用被拒绝。
5. PostgreSQL query profile 绕过 MCP 分类直连的 INSERT/UPDATE/DELETE 和 schema DDL 均被拒绝，夹具不变。
6. 普通写入的 direct/elicitation/admin 队列行为符合策略；高危操作强制管理员审批。
7. 建库申请在缺 grant 或 provision 映射时审批前拒绝；批准、配额、登记、可见性和归档恢复符合定义。
8. 服务重启后 grants、审计和预留保留，旧批准和外部写不重放。
9. 从 Bun/TypeScript v1 完成一次可恢复迁移；活动配置无旧凭证/权限，旧 binary 无法获得 source。
10. 审计可查到首版所有关键操作的记录，且不包含凭证。

## 8. 尚未决定的产品事项

以下事项不阻塞本 PRD 批准，但必须在对应实现任务开工前确定：

- 默认连接数、语句超时、token 请求速率和返回行数上限的具体数值。
- 审计和未知结果记录的保留周期、导出与清理边界。
- 建库名称生成规则、默认环境、非生产结果 grants 模板和账号创建策略。
- Web 管理页的信息架构和一次性登录令牌的有效期。
- 首版 PostgreSQL 最低支持版本和连接 TLS 策略。

## 9. 变更记录

产品范围、目标用户、成功指标或发布门禁变化时更新本文，并在 [CHANGELOG.md](../CHANGELOG.md) 记录。实现进度不写入本文。
