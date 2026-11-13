# mcp-db 规格

- 状态：当前基线
- 更新时间：2026-11-13
- 变更记录：[CHANGELOG.md](CHANGELOG.md)
- 实现状态：以 [TASK.md](TASK.md) 为准；本文件中的目标能力不等于已经实现

## 1. 范围与非目标

mcp-db 是一个常驻数据库服务器的 HTTP MCP 服务：统一接入多个 SQL 数据源，通过 **token 的 MCP tool 白名单 × token 的 source 权限 × source 自身策略** 控制 agent 能做什么、能在哪些库上做。产品边界是**保证不了数据写对，只保证写不出灾难**；数据库账号权限是最终安全边界，应用层分类、审批和配额提供纵深防御。

### 1.1 范围

- 单实例、多数据源的 Streamable HTTP MCP 服务；服务只监听 `127.0.0.1`，本机客户端通过 SSH 隧道接入。
- 驱动：PostgreSQL、MySQL/MariaDB、SQLite、SQL Server。各驱动的建库能力可分阶段实现，未支持时必须明确拒绝，不能退回通用 SQL。
- 每个 source 两套普通业务 DSN（只读、读写）与两个连接池；权限模式决定普通 SQL 使用哪个池。
- agent 身份来自 `Authorization: Bearer <token>`；token 由管理面签发，只保存哈希和识别信息，明文只在签发时返回一次。
- token 创建时同时分配：
  - 允许调用的 MCP tools；
  - 可访问的 source，以及每个 source 的 `readonly`、`readwrite` 或 `approval` 模式。
- 有效授权取三者交集：token 必须允许当前 tool、必须被分配目标 source、source 模式和 source 自身策略必须允许当前操作。服务端不能把工具不可见当作授权，调用时仍须再次校验。
- source 可标记环境（`dev`、`test`、`prod`）并由 admin 显式开启“允许 agent 申请建库”；默认关闭。生产 source 的普通 SQL 对 agent 始终只读，不能配置成有效写权限；专用建库是独立控制面动作，仍须经过开关、配额与逐次审批。
- agent 建库只能调用专用 `create_database` tool。每次申请均须人工审批，并受 source 开关、配额和频率限制；通用 SQL 不提供建库、删库或权限管理通道。
- source 模式继续固定三档：
  - `readonly`：查询走只读池；写入和 DDL 不执行；
  - `readwrite`：普通查询和普通写入可直接执行，高危操作仍须逐次审批；
  - `approval`：查询走只读池；每条普通写入和 DDL 执行前逐次审批，批准后该条语句走读写池。
- 普通 `approval` 写入的确认通道：优先 MCP elicitation；客户端未声明该能力时，进入待批队列。高危 SQL 和建库必须由 admin 身份在管理页或 CLI 做一次性决定；elicitation 可用于展示或通知，但客户端返回不能单独构成这两类操作的人工批准。
- 执行保护：参数绑定、返回行数截断、语句超时、连接池上限、建库配额与频率限制、审计。
- 管理面负责 token、source、审批、审计及 source 归档/恢复；agent token 不具备控制面生命周期权限。
- 单用户使用，不区分多个管理员、审批人角色或组织归属；不同 agent 可持有不同 token。

### 1.2 非目标

- 不解决逻辑污染；事务成功、权限合法的 SQL 仍可能写入错误业务数据。
- 当前不允许 agent 写生产 source；生产写入流程在真实需求出现后单独设计。
- 不允许 agent 通过通用 SQL 执行数据库级管理；`CREATE DATABASE` 只走专用 tool，`DROP DATABASE`、`GRANT`、`REVOKE` 不向 agent 开放。
- 不做完整工作区或共享工作区模型，不做 workspace 成员、角色、所有权、TTL、快照、一键恢复或自动删库；数据模型保留后续扩展空间。
- 不做团队/多租户、数据脱敏、列级授权或 RLS 管理；保密边界仍由数据库及部署环境负责。
- 不做跨驱动统一的存储空间和表数量硬限额；第一版只提供能够一致执行的连接、超时、返回行数、建库数量和建库频率限制。
- 不做限时写授权或常驻提权状态；token 的人工授权创建后不可修改。成功的 `create_database` 可按该次已审批请求自动给创建 token 增加新 source，这是建库结果的一部分，不允许借此修改其他 source 权限。
- 不支持 NoSQL；Oracle 只在驱动接口上留位置。
- 不做 stdio transport、IM 审批。
- 不替代数据库自身的备份、恢复、账号权限、审计和资源治理体系。

## 2. 授权与风险策略

### 2.1 判定顺序

每次调用按以下顺序判定；前置检查失败时不得继续执行、申请审批或触发数据库连接：

1. 认证 token，并确认未撤销；
2. 校验 token 的 tool 白名单；
3. 解析 source 并校验 token 的 source 分配；
4. 执行 source 自身策略，包括归档状态、生产只读和建库开关；
5. 对 SQL tool 使用对应方言的 fail-closed 解析识别查询、普通写入、高危、禁止和未知操作；多语句或未知语句直接拒绝；
6. 对普通 SQL 应用 source 的 `readonly`、`readwrite`、`approval` 模式；
7. 高危 SQL 无论 source 是 `readwrite` 还是 `approval` 都逐次审批；
8. 建库额外检查活动库配额与创建频率，再逐次审批。

`readwrite` 不能绕过高危审批，admin 开启建库也不能绕过 token tool 权限、source 分配、配额或逐次审批。

审批记录必须绑定不可变的请求摘要（请求类型、token、tool、source、规范化 SQL 与参数或建库参数的摘要），批准只能消费一次。等待审批期间不保留授权快照；批准后、执行外部写之前必须重新校验 token 未撤销、tool/source 授权、source 状态与策略、生产限制和配额。摘要不匹配、状态变化或重复消费都必须拒绝并审计。

### 2.2 SQL 风险分级

| 类别 | 语句 | 行为 |
| --- | --- | --- |
| 通用 SQL 禁止 | `CREATE DATABASE`、`DROP DATABASE`、`GRANT`、`REVOKE` | `query_sql`/`execute_sql` 在连接数据库前直接拒绝；建库只能走 `create_database`，其余不向 agent 开放 |
| 高危 | 库内对象的 `DROP`、`TRUNCATE`、`ALTER`、无 `WHERE` 的 `UPDATE`/`DELETE`、`CALL`/`EXECUTE` | 无论 source 是 `readwrite` 还是 `approval` 都逐次审批 |
| 普通写入与库内 DDL | `INSERT`、带 `WHERE` 的 `UPDATE`/`DELETE`、`CREATE TABLE/INDEX/VIEW` 等 | 按 source 模式处理 |
| 查询 | `SELECT`、只读元数据查询和不会执行被解释语句的执行计划 | 只允许 `query_sql`，走只读池 |

SQL 解析用于执行策略，是高危审批边界的一部分：必须按对应方言解析单条语句，多语句、解析失败或无法证明类别的语句一律拒绝，不能退回关键词猜测。数据库原生权限承担粗粒度的最终越权保护：普通只读/读写账号不得拥有建库、角色或授权管理权限。`query_sql` 只走只读池，避免通过副作用函数写入；存储过程和动态 SQL 入口一律归为高危；普通账号还必须无权通过 definer 函数、角色切换或其他间接入口取得数据库级管理能力。无法确认间接执行效果的调用直接拒绝，人工批准不能解除此限制。`EXPLAIN ANALYZE` 等会实际执行被解释语句的变体不属于 `explain_query`，必须通过对应的查询或写入 tool 并重新执行完整授权和风险判定。

### 2.3 Agent 建库

- `create_database` 的目标是一个已分配给 token、且由 admin 开启建库能力的 source；source 代表同一数据库服务上的建库入口，不额外引入 server 实体。
- source 的普通读写凭证不得拥有服务器级建库能力。建库使用独立 provisioner 凭证；该凭证不得进入普通连接池、不得由通用 SQL 使用、不得通过 API 或审计回显。
- agent 提供用途等受限参数，服务端负责生成或校验安全的数据库标识；不能接受任意 DSN、主机、环境或 provisioner 参数。新 source 的环境由 parent source 的 admin 预设策略决定，agent 不能自行把新库标为生产。
- 每个 token 在每个建库 source 下最多保有 5 个活动的 agent 创建库；每个 token 在每个建库 source 下每分钟最多成功创建 1 个库。归档后的新 source 不计活动库数量。admin 可配置更低上限，但不能取消逐次审批。
- 配额和频率先在进入审批前做快速检查；批准后、外部创建前必须在本地状态库事务内重新检查并原子预留活动库槽位和频率窗口。并发请求只有取得预留者可执行；执行前拒绝、取消、审批超时，或已证明物理创建未发生的失败须释放预留；执行结果未知或部分成功时保留隔离的预留，停止后续创建并由 admin 核对实际状态后结算，不能自动释放。恢复已归档的新 source 也必须重新满足活动库配额。
- 新库成功登记后，创建 token 按新 source 的环境获得权限：非生产 source 为 `readwrite`，生产 source 为 `readonly`；这是已审批建库请求的结果，不代表 token 可以修改其他 source 授权。其他 token 默认不可见，须由 admin 另行签发或授权。
- `DROP DATABASE` 不提供 agent tool。物理数据库删除由人通过数据库运维流程执行，不由 source 归档代替。

### 2.4 Source 生命周期

- source 归档和恢复只属于 admin 管理面，不作为 MCP agent tool 暴露。
- 归档会阻止新请求、关闭对应连接池并从 agent 可见列表中移除，但保留 source 配置、审批和审计记录，不删除物理数据库。
- 恢复由 admin 显式执行，恢复前重新校验配置、凭证和活动库配额；agent 不能归档、恢复或删除 source。

## 3. 需求与验收

本节使用 `AC2-*` 作为 2026-11-13 目标基线的独立验收编号。T-01 引用的 `AC-01`～`AC-11` 仅属于 2026-11-11 基线，其证据不自动证明本节要求；旧编号及证据保留在 T-01 任务与变更记录中，不在当前验收编号上复用。实现进度和证据位置以 `TASK.md` 为准。

| 编号 | 需求 | 验收标准 |
| --- | --- | --- |
| `AC2-01` | agent 身份与 source 可见性 | 缺少、未知或已撤销 token 时 `/mcp` 返回 401；`list_sources` 只返回该 token 配置且未归档的 source；工具的 `source` 参数只接受该 token 可访问的 source。 |
| `AC2-02` | `readonly` 无法写入 | `readonly` 的 token 不能通过普通 SQL 修改数据或 schema；写语句不执行，库内数据与 schema 不变。 |
| `AC2-03` | `readwrite` 普通读写 | 非生产 source 的 `readwrite` token 可直接执行普通写入；高危 SQL 仍按 `AC2-16` 审批。 |
| `AC2-04` | `approval` + elicitation 逐条确认 | 客户端声明 elicitation 能力时，每条普通写语句触发一次确认；accept 后执行，decline/cancel 后不执行；查询不触发确认。 |
| `AC2-05` | `approval` + 管理面兜底 | 客户端未声明 elicitation 能力时，写语句进入待批队列；admin 批准后执行，拒绝或超时后返回错误且不执行。 |
| `AC2-06` | 参数绑定 | SQL 只通过 `params` 传参；参数中的引号、分号不改变语句结构。 |
| `AC2-07` | 结果集截断 | 结果行数超过 source 的 `max_rows` 时最多返回 `max_rows` 行，并显式标记结果被截断。 |
| `AC2-08` | 语句超时 | 超过 `statement_timeout_ms` 的语句返回超时错误，连接池不被挂死，后续请求可正常执行。 |
| `AC2-09` | 审计 | SQL 调用、审批及拒绝写入可定位审计，包含 token/agent、tool、source、决定、渠道、原因、耗时和行数等适用字段。 |
| `AC2-10` | 管理面隔离 | 管理接口只接受 admin 身份；agent token 访问返回 401/403；服务只监听 `127.0.0.1`。 |
| `AC2-11` | 配置校验 | 非法 DSN、未知 source、非法模式、非法环境、生产写权限、非正数限制或建库开关缺少 provisioner 凭证时启动/保存失败，并指出字段路径。 |
| `AC2-12` | tool 白名单 | token 只能发现和调用创建时分配的 MCP tools；直接调用未授权 tool 也被服务端拒绝并审计。 |
| `AC2-13` | tool × source 双重授权 | 拥有 tool 但未分配 source，或拥有 source 但未分配 tool 时都不能执行；任一维度不能隐式扩大另一维度。 |
| `AC2-14` | 生产 source 只读 | 标为 `prod` 的 source 对普通 SQL 只提供查询和元数据能力；任何写入或库内 DDL 均在数据库执行前拒绝。专用建库按 `AC2-17～AC2-20` 独立判定，不能写入现有生产库。 |
| `AC2-15` | 通用 SQL 策略 | `query_sql`/`execute_sql` 收到数据库管理语句、多语句、解析失败或未知语句时在数据库执行前拒绝并审计；普通只读/读写账号不具备建库、角色或授权管理权限。 |
| `AC2-16` | 高危 SQL 强制审批 | 库内对象的 `DROP`、`TRUNCATE`、`ALTER`、无 `WHERE` 的 `UPDATE`/`DELETE`、`CALL`/`EXECUTE` 在 `readwrite` 与 `approval` 下均进入 admin 认证的逐次审批；批准绑定精确请求摘要且只能消费一次，拒绝、取消、超时、摘要不符或执行前复核失败均不执行。 |
| `AC2-17` | source 建库开关 | token 即使拥有 `create_database`，目标 source 未显式开启建库时也在审批前拒绝；开关默认关闭。 |
| `AC2-18` | 建库逐次审批与专用凭证 | 每次符合条件的 `create_database` 都进入 admin 认证的人工审批；批准绑定精确请求摘要且只能消费一次，执行前重新校验全部授权与策略；通过后只由隔离 provisioner 执行，agent 和普通 SQL 不能获取或使用该凭证。 |
| `AC2-19` | 建库配额 | 每 token × 建库 source 最多 5 个活动库且每分钟最多成功创建 1 个；批准后、创建前必须原子复核并预留，超限或并发竞争失败不执行；确认未创建才释放预留，未知/部分成功保留隔离预留待人工核对，恢复归档 source 也受活动库配额约束。 |
| `AC2-20` | 建库登记与授权 | 建库成功后登记新 source、创建 token/agent、父 source、环境和审计；创建 token 对非生产新 source 自动获得 `readwrite`，对生产新 source 只获得 `readonly`。跨边界部分成功必须停止并进入人工处置，不得暴露未就绪 source 或谎报成功。 |
| `AC2-21` | admin-only 归档与恢复 | 只有 admin 可归档和恢复 source；agent 无对应 tool，直接访问管理接口被拒绝；归档不删除物理库且保留审计。 |

需要端到端验证的新增 AC 及检查点：

- `AC2-12/AC2-13`：签发不同 tool/source 组合的 token → 建立 MCP 会话 → 检查工具发现结果 → 直接调用未授权组合 → 确认拒绝且数据库无请求、审计有记录。
- `AC2-16`：使用 `readwrite` token 提交高危 SQL → 进入 admin 待批队列 → 分别批准和拒绝 → 批准后篡改请求或撤销 token 再执行 → 确认只有请求摘要一致且执行前复核通过的批准分支执行一次。
- `AC2-18～AC2-20`：并发调用 `create_database` → 校验 source 开关与初次配额 → admin 分别批准、拒绝 → 执行前原子复核和预留 → provisioner 实际创建 → 新 source 按环境登记并只对创建 token 可见；另跑超限、执行失败和恢复超额路径，确认配额不被穿透、确定未创建时释放预留、未知或部分成功保留隔离预留并进入人工处置。
- `AC2-21`：admin 归档 source → 新请求被阻止且连接池关闭 → 在配额允许时由 admin 恢复；agent 调用管理接口始终失败，物理数据库未删除。

## 4. 架构边界与关键决策

- 系统边界：内部组件是配置、HTTP transport、身份鉴权、tool 授权、工具层、SQL 策略、连接池注册表、方言驱动、建库 provisioner、审批、配额、审计/状态存储、管理接口和 CLI；外部依赖是 MCP 客户端、数据库、人工审批者和凭证存储。
- 部署与运行形态：单进程常驻数据库服务器，Bun 直接运行 TypeScript；MCP 端点 `/mcp`，管理端点 `/admin/*`；只绑 `127.0.0.1:8787`，本机通过 SSH 隧道接入。状态库默认 `./.mcp-db/state.db`。
- 工程基线：Bun、TypeScript、ESM；单机部署。具体版本以锁文件和工程配置为准。

组件与职责（目标边界；实现进度以代码和 `TASK.md` 为准）：

| 组件 | 职责 |
| --- | --- |
| `src/config.ts` | 解析服务、source 策略、配额及凭证引用并校验跨字段约束 |
| `src/auth.ts` | Bearer token 解析、token/tool/source 授权与 admin 身份区分 |
| `src/transport/http.ts` | Streamable HTTP 接入、MCP 会话、能力协商、管理路由挂载 |
| `src/tools/*.ts` | MCP tool 定义、zod schema、调用时再次鉴权 |
| `src/core/pools.ts` | 按 source × role 建普通池；provisioner 不进入普通池注册表 |
| `src/core/sql.ts` | 按方言 fail-closed 解析及风险识别，承担高危审批策略边界；数据库权限仍承担最终越权保护 |
| `src/core/provisioning.ts` | 建库参数约束、配额、审批后执行、source 原子登记和创建者授权 |
| `src/drivers/*.ts` | 各方言元数据、查询、执行和可选建库适配 |
| `src/core/approval.ts` | 普通写确认与 admin 高危/建库审批；不可变请求摘要、一次性决定和执行前复核 |
| `src/core/store.ts` | source、token、tool/source 授权、审批、配额计数与审计持久化 |
| `src/core/audit.ts` | 审计记录写入；安全关键拒绝和建库结果必须可定位 |
| `src/admin/*` | token、source、建库开关、审批、审计、归档和恢复 |
| `src/cli/*` | 本机 CLI：serve、approvals、approve、deny 等管理操作 |

关键取舍：

1. **HTTP 而非 stdio**：服务在数据库服务器，本机客户端通过隧道接入。
2. **工具能力与数据范围正交**：token 的 tool 白名单回答“能做什么”，source 分配和模式回答“能在哪里做”；必须同时通过。
3. **数据库权限是最终边界**：只读、读写和 provisioner 凭证隔离；应用分类、工具隐藏和审批不能替代数据库原生授权。
4. **建库是专用能力**：source 兼任同一服务的建库入口，不提前引入 server 实体；通用 SQL 不能获得数据库级管理能力。
5. **审批不等于授权**：高危 SQL 和建库必须审批，但审批不能绕过 tool、source、生产只读、开关和配额检查。
6. **token 通常不可变**：调整人工分配须撤销重签；建库成功后的创建者授权是已审批结果的原子组成部分。
7. **归档可逆、删除外置**：管理面只管理 source 生命周期，不代替数据库物理删除和恢复流程。
8. **工作区后置**：当前用 `created_by`、父 source、环境和生命周期字段保留演进空间，不提前实现 workspace 权限体系。

## 5. 对外契约与数据模型

### 5.1 MCP tools

机器可读形状以 `src/tools/` 中的 zod schema 为准；实现新增工具时同步更新 schema。以下只规定非机器可读语义：

| tool | 典型授权 | 说明 |
| --- | --- | --- |
| `list_sources` | 基础 | 仅返回 token 被分配且未归档的 source，不返回任何 DSN |
| `list_tables` | 元数据 | 表/视图清单；source 必须可访问 |
| `describe_table` | 元数据 | 列、类型、可空、默认值、主键、索引、外键 |
| `explain_query` | 查询 | 只提供不会执行被解释语句的执行计划；`ANALYZE` 等执行型变体必须改走对应查询/写入 tool |
| `query_sql` | 查询 | 只接受只读 SQL，始终走只读池 |
| `execute_sql` | 写入/库内 DDL | 不接受数据库级管理；普通写按 source 模式，高危写强制审批 |
| `create_database` | 建库 | source 开关、配额、频率与逐次审批全部通过后，由隔离 provisioner 建库并登记新 source |

工具只对被 token 白名单允许的会话注册/展示；无论是否展示，服务端都必须在调用时重复鉴权。管理面的归档、恢复、token 和 source 管理不是 MCP agent tools。

### 5.2 管理接口

管理接口仅监听 `127.0.0.1`，只接受 admin Bearer 或由 admin 登录建立的安全会话。管理面至少承担：token tool/source 分配、source 策略与建库开关、待批决定、审计查询、source 归档与恢复。具体机器契约随实现提供，不在本文件复制。

### 5.3 配置语义

- source 至少包含 `id`、驱动、环境、普通只读/读写 DSN、限制参数、是否允许 agent 建库及可选 provisioner 凭证引用。
- 开启 agent 建库时必须配置对应驱动支持的 provisioner 凭证；该凭证与普通 DSN 隔离且 API 永不回显。
- token 至少包含 agent 标识、tool 白名单和 source→模式映射；生产 source 不能分配有效写模式。
- 密钥、DSN 和 token 明文不得写入仓库、审计或普通日志。具体配置字段名由后续实现任务的机器契约确定。

### 5.4 数据模型

| 表/概念 | 关键字段或语义 |
| --- | --- |
| `sources` | source 身份、driver、environment、状态（active/archived）、普通 DSN、provisioner 凭证引用、建库开关、父 source、`created_by_token/agent`、创建来源、限制参数、时间戳 |
| `tokens` | agent、token hash、tool 白名单、状态、创建/撤销时间；明文只在签发响应出现一次 |
| `token_sources` | token、source、模式；唯一约束保证每个组合只有一条有效授权 |
| `approvals` | 请求类型（普通写/高危 SQL/建库）、token/agent、source、tool、不可变请求摘要、状态、渠道、admin 决策者、一次性消费状态、过期与决定时间 |
| `audit` | 时间、token/agent、tool、source、请求摘要、决定、渠道、admin 决策者、原因、耗时、行数及适用的生命周期信息 |

建库成功时，物理创建、新 source 登记和创建 token 授权应尽可能形成可恢复的单一业务操作；数据库不支持跨边界原子事务时，失败必须停止后续步骤、记录实际状态并进入人工处置，不能盲目重试或谎报成功。

### 5.5 错误与审计语义

- 缺失、未知或已撤销 token：HTTP 401。
- tool 未授权、source 未分配、生产写入、source 已归档、source 未开启建库、禁止 SQL、quota/rate limit 超限、admin-only 操作：在外部写前拒绝，返回可区分但不泄露凭证的错误并写审计。
- 审批拒绝、取消或超时：工具返回拒绝错误，不执行外部写。
- provisioner 或数据库返回未知/部分成功：停止自动重试与后续写入，保留脱敏证据并报告人工处置需求。
- 审计至少区分 `allowed`、`rejected`、`error`，并能通过 reason 区分 tool、source、production、forbidden_sql、approval、quota、archive 等原因。

## 6. 未决问题

| 问题 | 影响 | 处理时点 |
| --- | --- | --- |
| 各驱动建库、建账号和 source 登记的补偿策略 | 物理建库与本地状态库不能跨边界原子提交，需定义失败后的人工处置与安全重试 | 建库实现任务设计阶段 |
| 各驱动首批支持范围 | 未支持驱动必须明确拒绝，不能借通用 SQL 绕过 | 建库实现任务拆分前 |
| 长 SQL 和建库请求在审批界面的摘要与详情 | 影响审批者能否识别目标、风险与参数 | 审批 UI 实现前 |
| agent 创建库的长期存储监控和回收 | 当前只限制活动库数量与创建频率，不限制跨驱动存储大小 | 工作区/资源治理迭代 |

## 7. 维护规则

1. 只在用户可见行为、对外契约、数据模型、架构边界变化时更新本文件，并在 `CHANGELOG.md` 记一条。
2. 任务级验收微调写任务或提交，不上升为基线。
3. 超过约 300 行时按领域拆分到 `spec/<domain>.md`，保留一个索引。
4. 同一主题只保留一个权威正文；历史材料不进入本文件。
5. 产品意图、目标用户、场景和范围变化时更新可选的 `docs/PRD.md`；SPEC 不重复产品叙事。
6. 对外 API 的机器可读契约以链接的 OpenAPI/schema 为准；本文件只保留链接、适用范围和非机器可读约定。
