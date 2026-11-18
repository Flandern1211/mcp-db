# mcp-db 规格

- 状态：当前基线
- 更新时间：2026-11-16
- 产品意图、目标用户、场景与成功指标：[docs/PRD.md](docs/PRD.md)
- 变更记录：[CHANGELOG.md](CHANGELOG.md)
- 实现状态：以 [TASK.md](TASK.md) 为准；本文件中的目标能力不等于已经实现

## 1. 范围与非目标

mcp-db 是一个多 SQL 数据源的 HTTP MCP 服务，可在任何能够访问目标数据库的受信位置按需启动或常驻运行。核心模型是：**token 在每个 source 上获得哪些 MCP tools；tool 使用哪个连接 profile 执行；执行前适用什么审批策略**。三者分别回答“能不能”“用什么数据库身份”“要不要批准”，不重复表达授权。

### 1.1 范围

- 单实例、多数据源的 Streamable HTTP MCP 服务；不要求与数据库同机，不要求全年常驻。
- 首版正式交付形态为 OCI 容器镜像（Docker 服务）。MCP 端口默认只发布到宿主回环地址或受控网络。
- 首版管理面仅限本机：管理页使用发布到宿主回环地址的独立端口，并要求 CLI 生成的一次性登录令牌；agent token 不能访问管理面。跨主机远程管理、TLS 终止和受信反向代理属于后续阶段，启用前必须同时具备认证和网络访问控制，不能裸露管理接口。
- 实现约定：管理监听默认 `127.0.0.1:8788`（`MCPDB_ADMIN_ADDR`），MCP 监听不变；容器内绑定 `0.0.0.0:8788` 时必须用 `-p 127.0.0.1:8788:8788` 发布，不能用 `-p 8788:8788` 暴露到非回环地址。一次性登录令牌有效期 5 分钟、只能消费一次，服务端只存其 SHA-256 hash；admin 会话保存在进程内，1 小时过期，重启即失效。本机 HTTP 下 `mcpdb_admin` cookie 设 HttpOnly、SameSite=Strict、Path=/admin 但不设 Secure；启用 TLS 后必须加 Secure。所有 admin 写请求校验 CSRF 与 `Origin`，Host 必须为回环地址。
- 首版正式发布范围只覆盖 PostgreSQL 普通能力；SQLite 是 T-11 的首个增量驱动，已实现并完成本地真实文件边界验证，尚未随首版发布。MySQL/MariaDB、SQL Server 按后续阶段扩展。未支持的驱动必须明确拒绝，不能退回通用 SQL。生产 source 的普通 SQL 只读。
- source 代表一个具体数据库，并配置一组连接 profiles。profiles 数量不固定为两个，可有多个最小权限连接；tool 通过服务端映射选取适用 profile，agent 不得指定 DSN 或切换执行身份。
- agent 使用管理面签发的 Bearer token；明文只在签发时返回一次，服务端只保存哈希和识别信息。
- token 创建时按 **source → tool 集合** 授权。不再维护独立的全局 tool 白名单与 source 模式，也不再使用 `readonly/readwrite/approval` 作为运行时权限枚举。
- 管理页可提供“只读”“数据读写”“结构管理”等快捷模板；模板只是勾选 tools 的便捷方式，保存结果是明确的 grants，不具有隐含继承关系。“逐次审批”是执行策略，不是权限模板。
- 普通 SQL 拆为 `query_sql`、`mutate_sql`、`execute_ddl`；通用 `execute_sql` 不属于目标 tool 集合，旧客户端必须迁移，不能保留可绕过新授权的兼容入口。
- 生产 source 的普通 SQL 对 agent 只读，不能授予 `mutate_sql` 或 `execute_ddl`。专用建库独立受控：目标 source 有 provision 能力、token 在该 source 有 `create_database` grant，并且每次由 admin 人工审批。
- 不设独立“允许 agent 建库”布尔开关；配置 provision profile 提供技术能力，tool grant 提供申请资格，admin 决定提供本次授权。
- 管理面负责 token、profiles、tool 路由、审批策略、审批决定、审计和 source 归档/恢复；agent 不具备这些控制面管理权限。
- 参数绑定、结果截断、执行超时、连接池限制、请求速率和建库配额负责限制影响面；数据库原生权限仍是执行身份的最终能力边界。
- 单用户管理，可供多个 agent/token 使用；不引入多个管理员角色或组织归属。

### 1.2 非目标

- 不承诺“所有错误都可拦截”或“写不出灾难”；审批和权限不保证业务语义正确，合法 SQL 仍可能写坏业务数据。
- 当前不开放 agent 写生产数据；生产写入在真实需求出现后单独设计。
- 不提供 agent 删库、账号/角色管理或权限管理。建库所需的账号配置仅由受控 provisioner 内部完成，不向 agent 暴露任意管理 SQL。
- 不做完整工作区/共享工作区、成员体系、TTL、快照、一键恢复或自动删库；保留创建者、父 source 和生命周期扩展空间。
- 不做团队/多租户、数据脱敏、列级授权或 RLS 管理；实际数据库授权仍由管理员负责。
- 不做跨驱动统一的存储空间和表数量硬限额；第一版限制连接、执行时间、返回行数、请求速率、建库数量和频率。
- token 人工分配创建后不可修改，变更须撤销重签；受审批的新库创建结果可以增加该 token 对新 source 的 tools，不允许借此修改其他 grants。
- 不做 NoSQL、stdio transport、IM 审批或分布式多实例部署；按需启动不等于无状态或 serverless。
- 不替代数据库备份、恢复、原生资源治理和审计。

### 1.3 驱动支持矩阵

状态表示能力是否已实现并通过对应边界验证，不表示已经随版发布。未列入“已支持”的驱动在状态层和执行层都拒绝，不退回通用 SQL。

| 驱动 | 状态 | 元数据/explain | 只读查询 | 数据写入与库内 DDL | 建库 |
| --- | --- | --- | --- | --- | --- |
| PostgreSQL | 首版已支持 | 支持 | 支持，原生只读会话与账号边界 | 支持 | 支持，仅 provision 审批路径 |
| SQLite | T-11 增量已实现，未发布 | 支持，schema 统一 `main` | 支持，文件只读打开加 `query_only` | 支持，mutate/schema profile | 不支持，`create_database` 明确失败 |
| MySQL/MariaDB | 未支持 | 拒绝 | 拒绝 | 拒绝 | 不支持 |
| SQL Server | 未支持 | 拒绝 | 拒绝 | 拒绝 | 不支持 |

## 2. 部署与运行生命周期

- 首版以 OCI 容器运行在个人电脑、开发机、内网管理机或数据库所在主机，只需能访问 PostgreSQL 并被 MCP 客户端和管理者访问。
- 使用期间 HTTP 服务须保持运行。单人可按需启动，多客户端可常驻；部署位置不改变授权与审批语义。
- source、profile 元数据、token/grants、审计、审批状态和建库预留须持久化；数据库连接池与 MCP 会话是进程内状态。
- 停机/重启不能自动执行旧批准或盲目重放写入。尚未执行的待批/已批请求终止，客户端需重新发起并重新审批；已经发出的写入若结果未知，保留未知状态并人工核对，不标记为未执行或重试。
- 首版管理面仅本机：MCP 与管理监听分离，管理端口只发布到宿主回环地址，登录使用 CLI 生成的一次性令牌；admin 会话须有 HttpOnly、SameSite 和 CSRF 防护。跨主机接入属于后续阶段，启用前必须明确公开访问地址、受信代理及 Origin 校验配置，并在 HTTPS 下使用 Secure cookie。

## 3. 授权、连接与审批

### 3.1 Per-source tool grants

每条 grant 是 `(token, source, tool)`，默认拒绝，并可为普通写入携带 `direct/approval` 执行策略；策略只决定已有 grant 如何执行，不赋予 tool 能力。source 不因另一个 source 上已有相同 tool 而开放；拥有查询不隐含写入，拥有 DDL 不隐含查询或建库。

- 会话工具列表是该 token 在所有有效 source 上 grants 的并集；调用时必须再次校验当前 source 上的精确 grant，不能因 tool 已在别处可见而放行。
- `list_sources` 是认证后的基础发现工具，只返回至少存在一个 grant 且未归档的 source；不额外维护全局白名单，返回结果不能扩大任何 tool 权限。
- source 选择参数只包含该 token 对当前 tool 有 grant 的有效 source；外部直接构造未授权参数同样拒绝。
- 新建 source 自动授权是明确例外，须更新当前会话的 source 授权视图；tool 集合变化时通过协议通知或要求重建会话后发现，不能让静态参数枚举永久阻止访问新 source。

### 3.2 Connection profiles

每个 source 可配置任意数量的命名 profiles；每个 profile 有用途类别、凭证引用、连接限制和启用状态。数量不限于两个，同类别可有多个 profile。每个 tool 绑定一个明确 profile；找不到可用映射时拒绝，不能自动升级到权限更大的身份。PostgreSQL 是首版发布基线；SQLite 在 T-11 完成 query profile 负向文件边界验证后启用；MySQL/MariaDB、SQL Server 在对应扩展任务完成前不得启用。

| 用途类别 | 用途 | 典型 tool |
| --- | --- | --- |
| `query` | 元数据和受限查询 | `list_tables`、`describe_table`、`explain_query`、`query_sql` |
| `mutate` | 数据写入 | `mutate_sql` |
| `schema` | 库内结构操作 | `execute_ddl` |
| `provision` | 同一数据库服务上的受控建库及必要账号配置 | `create_database` |

普通 profiles 不能拥有数据库级建库、角色/授权管理权限，也不能通过 definer 函数、角色切换等间接入口取得这些能力；provision profile 必须与普通执行路径隔离，不进入普通 SQL 可选连接集合。

同一 DSN 可以供多个普通 profiles 引用，但这不产生数据库级隔离。管理页须提示复用凭证的风险；`query` 的实际只读性必须由数据库权限、受支持的只读会话/事务或对应驱动的只读打开方式实现，不能仅靠 profile 名称保证。每个已发布驱动都必须通过 query profile 直接尝试数据写入和 schema DDL（绕过 SQL tool 分类），由数据库/文件边界拒绝且夹具状态不变，才能启用该 profile；无法完成负向集成验证的 profile 不得用于查询路径。

agent 不拿到 DSN/数据库密码；凭证由服务端安全存储引用并永不回显。profile 只能由 admin 配置和停用，token/grant 不赋予管理 profile 的权限。

### 3.2.1 首版 PostgreSQL 资源与只读机制

- 未单独配置时每 profile 最多 5 个并发数据库连接，语句超时 30,000ms；SQL 与元数据共享 source/profile 额度，达到上限立即拒绝，不排队或升级身份。连接建立与会话初始化分别有 10s 客户端期限。
- profile 可配置 `max_connections`（1～1000）和 `statement_timeout_ms`（1～2147483647）；超界保存失败。当前按请求建立连接，调用结束关闭连接；不承诺复用空闲连接。
- 每个 query profile 的实际连接在交给工具前设置 PostgreSQL 原生只读默认值，直测空谓词 DML 和回滚保护下的 DDL，只有数据库明确拒绝后才放行。查询及 explain 另外在显式只读事务内运行；连接关闭即回滚。数据库账号仍须遵守最小权限，不能由应用只读设置替代账号权限治理。
- token 普通工具调用使用滑动 60s 窗口，默认 60 次，可用 `MCPDB_TOKEN_REQUESTS_PER_MINUTE` 设置为 1～100000；同一 token 所有会话共享，达到上限返回 `RATE_LIMITED` 并审计。初始化、tools/list 和协议通知不计入工具调用额度；重启清空普通限流窗口，不影响持久建库配额。
- MCP 取消会关闭进行中的连接；已发出写入取消保守记录未知，不自动重试。连接额度拒绝发生在外部语句发送前，记录明确未执行，而不是未知。

### 3.2.2 SQLite 资源与只读机制

- source 的 `driver` 为 `sqlite`；profile 的 `credential_ref` 仍是 `keyring:...`，解析结果只作为数据库文件路径使用，不按 DSN 或 URI 解析。空路径和 `:memory:` 明确拒绝；文件不存在时打开失败，不自动创建。
- query profile 以 `SQLITE_OPEN_READ_ONLY` 打开文件并设置 `PRAGMA query_only=ON`。连接交给工具前读取 `PRAGMA query_only` 必须为 1，并对已有表实际执行零行 DML、对 main schema 执行 `CREATE TABLE` 探针（空库用 `BEGIN IMMEDIATE`）；只有 SQLite 返回只读或权限错误才放行，否则 profile 不可用。
- mutate 与 schema profile 以读写方式打开同一文件。SQLite 没有建库能力，`create_database` 对 sqlite source 在审批与执行前明确失败；provision profile 仍不能进入普通 SQL 与元数据路径。
- 每次调用先经 profile 连接额度准入，调用结束关闭连接；连接上限和默认值与 PostgreSQL 相同。语句超时用 SQLite progress handler 按 deadline 中断并映射为资源限制；`max_rows` 的 `rowCount`/`returnedRows`/`truncated` 语义与 PostgreSQL 一致。
- 元数据只读打开，表和视图来自 `sqlite_master`（排除 `sqlite_*`），列、主键、索引和关系来自 `pragma_*` 表函数，schema 统一为 `main`。explain 只接受生成的 `EXPLAIN QUERY PLAN`。

### 3.3 调用判定与审批

1. 认证 token，确认未撤销；
2. 解析目标 source，确认有效且该 token 在此 source 上有当前 tool grant；
3. 检查生产限制，解析当前 tool 的服务端 profile 映射并确认启用；
4. SQL 按方言 fail-closed 解析，拒绝不属于该 tool 的语句、多语句、禁止或未知操作；
5. 检查请求速率及适用配额；
6. 应用 source/tool 审批策略，高危 SQL 和建库不能降低为直接执行；
7. 外部执行前复核授权、source/profile 状态与版本、环境和策略；需要配额预留的操作须原子完成预留。

普通写入的 grant 可由 admin 配置 `direct` 或 `approval`，新增 grant 默认 `approval`；source/tool 路由还能设置不可下调的最低策略，实际策略取 grant、路由最低策略和内置强制规则中的更严格者。`direct` 不跳过高危审批。普通审批可通过受信客户端的 MCP elicitation 交互确认，不支持时转 admin 待批队列；它只是交互确认，不是独立的人类身份凭证。高危 SQL 与建库必须由 admin 在管理页或 CLI 决定，不能仅凭 agent 客户端的 accept。

批准必须绑定精确不可变请求摘要（token、source、tool、完整操作参数以及解析后的执行目标/profile 版本），只能消费一次。等待期间撤销 token、归档 source、停用 profile、变更路由/策略或请求内容，都使旧批准失效。未知结果禁止自动重试。

### 3.4 SQL 工具与风险

| 类别 | 行为 |
| --- | --- |
| 查询 | `query_sql` 只允许只读语句并走有效只读 profile；副作用调用、写 CTE 等不能混入 |
| 数据变更 | `mutate_sql` 只接受数据变更，不接受结构或数据库级管理；普通写按审批策略 |
| 库内 DDL | `execute_ddl` 处理表、索引、视图等库内对象；不能管理数据库、账号或权限 |
| 高危 | 库内 `DROP`、`ALTER`、`TRUNCATE`、无 `WHERE` 的 `UPDATE/DELETE` 总是 admin 审批；`TRUNCATE` 归 `mutate_sql` |
| 间接执行 | `CALL/EXECUTE` 仅在能确认操作范围属于数据变更时归 `mutate_sql`，且总是 admin 审批；不能确认效果或可能绕过数据库管理禁令时直接拒绝 |
| 禁止 | 所有通用 SQL tools 禁止 `CREATE/DROP DATABASE`、账号/角色管理和 `GRANT/REVOKE`；包含方言等价形式，MySQL 的 `CREATE/DROP SCHEMA` 按数据库级操作处理 |

单条 SQL 按方言解析，未知、多语句、解析失败或不能确认间接效果一律拒绝，不能退回关键词猜测。SQL 解析承担 tool 范围与审批策略边界，数据库最小权限承担执行身份的最终能力边界。

SQLite 使用独立方言解析，不接受 PostgreSQL 解析结果。下列语句即使语法可解析也在外部执行前拒绝：多语句、`ATTACH`/`DETACH`、`PRAGMA`、`VACUUM`、`REINDEX`、`ANALYZE`、`load_extension`、触发器、虚拟表，以及 SQLite 不支持的 `TRUNCATE`。query、mutate、库内 DDL 的类别边界与 PostgreSQL 相同；`EXPLAIN QUERY PLAN` 只由服务端基于已验证查询生成，原始 `EXPLAIN` 输入不接受。MySQL/MariaDB 与 SQL Server 的方言和工具范围仍未实现。

`explain_query` 仅允许只读查询的非执行型计划，不接受 `ANALYZE` 等实际执行变体或写语句。不能用执行计划入口绕过 `query_sql`/`mutate_sql` grants。`WHERE` 存在不代表影响小，例如 `WHERE 1=1` 仍可改全表，普通写入不能因此宣称安全。

### 3.5 Agent 建库

- 不额外引入 server 实体。source 兼任同一数据库服务的建库入口，必须存在启用的 provision profile 映射，且 token 在该 source 上获授 `create_database`。
- 不再有独立建库开关。缺少 provision 映射/凭证或 tool grant 时，在审批前拒绝；有资格也必须每次经过 admin 审批。
- agent 只提供用途等受限参数；实际名称、主机、凭证、环境和新 source 配置由服务端生成或 admin 预设，不能由 agent 注入任意 DSN 或服务器参数。
- 审批展示创建目标、数据库名称、环境、所用 profile 身份摘要，以及将授予创建 token 的新 source tools，全部绑定批准摘要。
- 新库需配置适用的 query/mutate/schema profiles，登记父 source、创建 token/agent、环境和审计。非生产新库按 admin 预设模板授予显式 tool 集合；生产新库只授予查询/元数据 tools。新库不继承 provision 能力或 `create_database` grant，不能递归建库绕过配额。
- 新 source grants 与本地登记事务一致；尚未就绪的 source 不可见。物理建库和本地登记不能跨边界原子提交时，部分成功必须隔离并由 admin 核对，不能谎报成功或自动清理未知结果。
- 每 token × 父建库 source 默认最多 5 个活动创建库，每 60 秒最多成功创建 1 个；admin 可收紧。只统计该父 source 创建结果，别名 source 和重签 token 不构成服务器全局存储限额。
- 审批前检查，执行前原子复核并预留数量和频率窗口；并发竞争失败不执行。归档不计活动配额，恢复须重新占用；归档不会释放物理存储，不能据此承诺磁盘安全。
- 执行前取消/拒绝/审批超时，或已证明未创建时释放预留；创建结果未知/部分成功保留隔离预留，阻止该 token × 父 source 后续创建，待 admin 核对结算。
- `DROP DATABASE` 不向 agent 开放。物理删除由人经数据库运维流程执行，source 归档不能替代删库。

首版 PostgreSQL 实现约定：

- 建库规格由服务端生成并持久绑定：数据库名形如 `mcpdb_<np|prod>_<10位随机>`，环境继承父 source；agent 只能提供父 source 与可选标签。规格写入审批请求参数摘要，admin 批准后重试只凭审批 id 与 token 消费已存规格，不重新生成名称。
- 每库独立数据库身份：provision 账号建库后创建 `<db>_ro`（仅 SELECT）与 `<db>_rw`（可创建库内对象）两个 LOGIN 角色，口令随机生成；query/explain 使用 `_ro`，mutate/schema 使用 `_rw`。数据库 owner 保留给 provision 账号，普通 profile 不复用该身份。DSN 写入加密凭证库并在写后热加载；DSN、口令不进入审批摘要、审计或日志。provision 管理凭证限定在 `keyring:provision/` 命名空间，普通 profile 不得引用。
- 结果 grants 由模板决定：非生产默认只授予只读 tools，可用 `MCPDB_PROVISION_RESULT_TOOLS` 收紧或扩展；生产新库一律只读。结果 grants 永不含 `create_database`。
- 配额参数：`MCPDB_PROVISION_MAX_ACTIVE`（默认 5）与 `MCPDB_PROVISION_MIN_INTERVAL_MS`（默认 60000）可收紧数量与频率；数量按未归档子 source 统计，频率窗口计入已预留请求。
- 未知与部分成功：数据库已创建但角色、凭证或本地登记失败时保留隔离预留，阻塞同 token × 父 source 后续建库，等待 admin 核对结算；服务器明确拒绝的语句视为未创建并释放预留。

### 3.6 Source 生命周期

归档和恢复仅限 admin，不是 MCP agent tools。归档阻止新请求、使待批请求失效、关闭连接池并从 agent 列表移除，但保留配置、grants 和审计，不删除物理数据库。已发出的请求不承诺可撤回，结果未知时按未知状态处理。恢复须复核环境、profiles、grants 和活动配额。首版通过本机 CLI 提供审批、归档/恢复和隔离预留结算；Web 管理面见后续管理任务。

## 4. 需求与验收

使用 `AC2-*` 表示本轮目标基线，条目会随尚未实现的设计修订。T-01 的 `AC-01～AC-11` 属于 2026-11-11 基线，其证据不自动证明本节；后续实现任务须固定引用对应基线提交。

首版发布范围只覆盖 PostgreSQL 普通能力、本机 CLI/Web 管理面和 Docker 交付。其他驱动与跨主机远程管理属于后续阶段；未被首版覆盖的 AC2 检查点必须保持未验证，不能算通过。

| 编号 | 需求 | 验收标准 |
| --- | --- | --- |
| `AC2-01` | 身份与发现 | 缺失、未知或撤销 token 返回 401；`list_sources` 只返回 token 至少持有一个 grant 且有效的 source，不返回凭证 |
| `AC2-02` | 只读查询 | 仅获查询 tools 的 token 不能写入数据/schema；每个已发布驱动（首版 PostgreSQL）须通过 query profile 直连负向集成测试，证明数据写入与 schema DDL 被数据库/文件边界拒绝且夹具不变，不能靠名称或 SQL 分类通过 |
| `AC2-03` | 普通写入 | 获相应 source/tool grant 且有适用 profile 时，非生产普通写按 grant、路由最低策略和内置规则中的最严格策略执行，无 profile 不回退 |
| `AC2-04` | 普通交互审批 | 普通写的 elicitation accept 才执行，decline/cancel 不执行；不是高危/建库的 admin 批准替代品 |
| `AC2-05` | admin 审批 | 不支持 elicitation 的普通审批及所有高危/建库进入 admin 队列；拒绝、取消、超时不执行 |
| `AC2-06` | 参数绑定 | 值参数通过绑定传递，不改变 SQL 结构；建库标识符由服务端生成/校验，不按值参数拼接任意管理 SQL |
| `AC2-07` | 结果限制 | 超过 source 行数上限显式截断；不把结果截断等同于限制数据库扫描成本 |
| `AC2-08` | 资源限制 | 每 profile 连接上限、语句超时、token 请求速率按配置执行；未支持的硬限制明确拒绝配置，不假装生效 |
| `AC2-09` | 审计 | 记录 token/agent、source、tool、profile 标识、决定、渠道、原因与适用耗时/行数，绝不记录凭证 |
| `AC2-10` | 部署与管理隔离 | 首版以 Docker 服务在非数据库主机按需启动；管理页仅通过宿主回环端口和一次性登录令牌访问；agent 不能操作 admin API；跨主机接入在后续阶段具备 TLS/认证/网络控制后才可启用 |
| `AC2-11` | 配置校验 | 未知 tool/profile、错误用途映射、生产写 grant、非法凭证引用或限制保存失败；连接数不固定为两个，query 实际只读性有验证依据 |
| `AC2-12` | 工具发现 | 展示 token 在有效 source 上 grants 的 tool 并集，加基础 `list_sources`；不可见不替代调用鉴权 |
| `AC2-13` | 精确 grant | A 库获某 tool 不允许对 B 库调用该 tool；直接构造参数也拒绝；source/profile/环境限制始终生效 |
| `AC2-14` | 生产只读 | prod source 拒绝 `mutate_sql/execute_ddl` grants 及调用；专用建库独立审批，不修改既有生产数据 |
| `AC2-15` | SQL 范围 | query/mutate/DDL 严格分离；数据库级管理、未知、多语句或解析失败在外部执行前拒绝；explain 不能执行 SQL |
| `AC2-16` | 高危策略 | 高危 SQL 必须 admin 审批，direct 不豁免；摘要不符、撤销、路由/策略变化和重复消费都不执行 |
| `AC2-17` | 建库能力 | create_database 必须同时有目标 source 的精确 grant 和启用 provision 映射；无重复布尔开关，缺任一条件审批前拒绝 |
| `AC2-18` | 建库审批 | 每次建库由 admin 批准并绑定创建目标、profile 版本和结果 grants；一次性消费，执行前复核；provision 凭证不能进入通用 SQL |
| `AC2-19` | 并发配额 | 默认每 token × 父 source 5 个活动库、60 秒成功 1 个；执行前原子预留，恢复受限；未知结果保留隔离预留待核对 |
| `AC2-20` | 新库登记 | 就绪后才暴露新 source 与结果 grants；prod 仅查询，非 prod 按审批模板；不继承建库能力；现有会话可重新发现新 source |
| `AC2-21` | 归档恢复 | 仅 admin 操作，阻止新请求、使待批失效、保留记录且不删物理库；恢复复核 profiles、grants 和配额 |
| `AC2-22` | 按需停启 | 重启保留配置/grants/审计/预留；不重放旧批准或外部写；未发出请求终止，已发出未知请求保留待核对状态 |
| `AC2-23` | 旧模型迁移 | 停服后按固定矩阵导入 TOML 与状态库，用 journal 完成可恢复 cutover，不生成建库能力；未分类 source 禁用。提交后活动 TOML 不含旧凭证/权限，旧 binary 用活动配置无法获得 source；新服务只启用分类 tools 并拒绝旧 `execute_sql`，失败/崩溃只能回滚提交点前状态或进入 recovery |

端到端证据检查点（不是本次文档变更的运行结果）：

- `AC2-02`：每个已发布驱动（首版 PostgreSQL）用 query profile 绕过 MCP SQL 分类直接尝试 `INSERT/UPDATE/DELETE` 与 schema DDL → 数据库或文件只读边界全部拒绝 → 夹具数据和 schema 不变；随后正常查询仍成功。
- `AC2-12/AC2-13`：签发 A/B source 不同 tool grants → 建立 MCP 会话看工具并集 → 在未授权 source 调用可见 tool → 确认拒绝且目标数据库无请求。
- `AC2-15/AC2-16`：提交跨 tool SQL、执行型 explain、禁止/未知语句和高危 SQL → 前者拒绝、后者进入 admin 队列 → 批准后变更策略/撤销 token → 确认复核失败且不执行；有效批准只执行一次。
- `AC2-17～AC2-20`：检查 provision 与 grants → 并发申请 → admin 分别批准/拒绝 → 原子预留 → 实际创建 → 登记新 source 与结果 grants → 验证创建者访问及其他 token 不可见；失败、未知、恢复超额和递归建库路径分别有数据库/状态库证据。
- `AC2-10/AC2-22`：服务从非数据库主机启动并访问多个库 → 提交待批及已发出请求 → 停启服务 → 验证持久化、旧请求不重放、未知状态保留；远程入口验证 TLS、认证和 admin 隔离。
- `AC2-23`：复制旧 TOML 与状态库 → 停服并取得维护锁 → staging 导入与校验 → 在各 cutover 点注入崩溃 → 验证 journal 只能完成、提交点前回滚或进入 recovery，绝不启动混合授权；成功后检查活动 TOML 无旧凭证/权限、无建库 grant、未分类 source 禁用、新服务拒绝 `execute_sql`，并用旧 binary + 活动配置确认得不到 source/agent 权限。

## 5. 架构边界与数据模型

### 5.1 组件与工具

单进程 Rust 服务；HTTP 端点 `/mcp`、管理页 `/admin` 与管理接口 `/admin/*`。MCP 监听与管理监听分离，首版管理监听只绑定或发布到宿主回环地址。默认状态库 `./.mcp-db/state.db`。首版正式交付形态为 OCI 容器镜像；具体 crate、Rust 版本和镜像版本以配置与锁文件为准。

下表是 Rust 模块职责边界，路径为职责示意，不提前锁定具体 crate 或文件布局。

| 组件 | 目标职责 |
| --- | --- |
| 配置与校验 | 部署、profiles、路由、审批策略、限制及跨字段校验 |
| 身份与授权 | token 身份、精确 source/tool grant、admin 区分 |
| HTTP/MCP transport | MCP 会话/发现、本机管理边界、生命周期 |
| tools | 工具 schema、调用鉴权与操作类别约束 |
| 连接管理 | 按 source/profile 建池，禁止未配置回退与 provision 越界 |
| SQL 策略 | 按已发布驱动方言解析，约束工具范围、禁止及高危操作 |
| provisioning | 建库审批、预留、方言执行、结果登记与异常核对 |
| approval | 请求绑定、一次性决定、执行前复核 |
| 持久化 | source/profile/grants、审批、配额、审计和迁移状态 |
| admin/CLI | 管理配置、审批、归档恢复和审计查询 |

机器可读契约以 Rust 源码中的 tool schema 定义为权威；服务实际 `tools/list` 输出必须与实现一致。本文不把目标工具表冒充已实现接口。

| tool | source profile 与约束 |
| --- | --- |
| `list_sources` | 无数据库连接；基础发现，返回已有 grant 的 source |
| `list_tables` | query profile；须有当前 source/tool grant |
| `describe_table` | query profile；结构与关系 |
| `explain_query` | query profile；只读非执行型计划 |
| `query_sql` | query profile；仅只读 SQL |
| `mutate_sql` | mutate profile；DML 与受控间接数据操作，高危不能绕过审批 |
| `execute_ddl` | schema profile；仅库内对象 DDL |
| `create_database` | provision profile；始终 admin 审批 |

### 5.2 数据模型与凭证

| 表/概念 | 语义 |
| --- | --- |
| `sources` | driver（`postgres`、`sqlite`；MySQL/MariaDB 与 SQL Server 未支持）、environment、active/archived 状态、父 source、创建 token/agent、创建来源、限制、时间戳 |
| `connection_profiles` | source、命名 profile、用途、凭证引用、连接限制、启用状态与版本；无固定两个的数量约束 |
| `source_tool_routes` | source、tool、profile、最低审批策略、版本；高危/建库强制规则不可下调 |
| `tokens` | agent、hash、状态、创建与撤销时间；不再有全局 tool 白名单 |
| `tool_grants` | token、source、tool 三元唯一及普通写入的 direct/approval 策略；不存 readonly/readwrite source 模式，策略不赋予 tool |
| `approvals` | token/agent、source、tool、精确请求摘要、profile/路由版本、状态、渠道、admin 决策身份、一次性消费和过期时间 |
| `quota_reservations` | token、父 source、请求、计数/频率预留、执行状态、未知/隔离标记 |
| `audit` | 时间、身份、source/tool/profile、摘要、决定、渠道、原因、耗时/行数及适用生命周期信息 |

凭证用系统密钥环或等价安全存储；持久状态记录引用，不保存真实密码/token 明文。profile 凭证不回显、不进入仓库、日志或审计。provision 必须与普通连接隔离。SQL/参数摘要也可能含业务数据，按敏感本地材料保护。

`quota_reservations` 以预留记录承载建库数量/频率额度和未知状态，状态为 `reserved`、`committed`、`released`、`quarantined`。加密凭证库由服务进程单写者原子覆盖并热加载；跨进程并发写入不在首版承诺范围。

### 5.3 兼容与错误

- 旧实现指 Bun/TypeScript v1。其 sources、两套 DSN 与 agent source 模式位于 TOML，审批/审计位于状态库；迁移目标是 Rust AC2 状态模型。后续完整档实现必须使用停机维护锁和持久 migration journal 做跨工件 cutover，不能把它描述成单个 SQLite 事务：先阻止 MCP/admin 业务请求，快照活动 TOML 与状态库，再导入 staging 状态并完整校验。
- 旧 source 的 `dsn_readonly` 转为 query profile，`dsn_readwrite` 转为 mutate 与 schema profiles；两者可以引用同一凭证但必须显示隔离告警。旧 source 没有 provision profile，也不会获得建库能力。
- 每个旧 token × source 按固定矩阵转换：`readonly` → 元数据、非执行 explain、`query_sql` grants；`readwrite` → 前述 grants + `mutate_sql` + `execute_ddl`，这两个写 grants 的普通策略为 direct；`approval` → 与 readwrite 相同 grants，但两个写 grants 的普通策略为 approval。source/tool 路由的最低策略按最保守值初始化为 direct，由各 grant 保留原有差异，高危内置规则仍不可下调。所有转换都不生成 `create_database` grant，旧固定 tool 集不存在可额外放宽的新权限。
- 旧 source 没有环境字段，迁移后保持 disabled，不接受业务调用；admin 必须先标记环境、验证 query profile 的真实只读边界并复核生成 grants/策略，才能逐个启用。管理员可删减迁移生成的 grants，但不能在迁移过程中隐式增加建库或生产写权限。
- 校验 staging 状态后，以 journal 记录提交点：激活新状态，并用同目录临时文件 + fsync + rename 替换当前生效 TOML，新的活动 TOML 不再包含旧 sources、agents、DSN 或旧 token 配置。原敏感配置移到非默认路径按凭证材料保护，不能由服务自动回退读取。跨文件切换中断时，新版本只能进入 migration recovery，不能启动 MCP；恢复根据 journal 完成或在提交点前回滚，不能猜测当前 source of truth。
- 切换成功后只注册分类 tools，旧 `execute_sql` 直接调用也拒绝。使用活动配置启动旧 Bun/TypeScript binary 时不得获得 source 或 agent 权限；切换后不支持自动降级。管理员故意恢复旧敏感备份并运行旧二进制属于显式回滚/凭证风险，必须停服、人工执行并重新评估数据库凭证，不能由新服务承诺阻止。
- 旧 Bun/TypeScript v1 源码和二进制在迁移验收完成前保留为迁移夹具和兼容测试对象，不作为正式开发入口；完成后是否移除由发布策略决定。
- 首版迁移入口为 `mcp-db migrate-legacy --config <legacy.toml> [--environment production|nonproduction]` 与 `mcp-db recover-legacy-migration`；状态库存在未 checkpoint 的 WAL 时拒绝迁移，production 与遗留写模式冲突时在写入任何目标文件前失败；迁移后 source 保持 archived，需 `mcp-db sources set-environment` 分类后才能恢复。
- 缺失/未知/撤销 token 返回 401；grant 不足、profile 缺失/停用、归档、生产限制、错误 SQL 类别、限流/配额或审批拒绝返回可区分的拒绝原因并审计，不泄露凭证。
- 审批和外部执行结果未知时停止自动重试、保留实际状态；外部副作用与本地登记不宣称跨库原子性。恢复步骤须在实现任务中明确验证。

## 6. 后续实现设计事项

| 事项 | 必须确定的内容 | 时点 |
| --- | --- | --- |
| 驱动与 SQL 解析 | PostgreSQL 首版范围已确定；SQLite 增量方言、文件只读与连接限制已由 T-11 定义；MySQL/MariaDB、SQL Server 在扩展任务前不得启用 | 对应实现任务拆分前 |
| 迁移与会话刷新 | Bun/TypeScript v1 TOML + 状态库的 journal/cutover/恢复、旧敏感配置隔离、固定 grants/策略转换、旧 binary 与 tool 退役、新 source 发现机制 | 权限实现前 |
| 配额和限流配置 | profile 池大小/超时、token 普通请求速率的默认值、重启恢复和未知状态核对 | 限制实现前 |
| 建库失败与凭证 | PostgreSQL 建库/账号配置补偿、凭证安全存储、admin 人工处置入口 | 建库实现前 |
| 本机管理面 | 一次性登录令牌、admin 会话 cookie、CSRF、一次性令牌有效期和 Web 信息架构 | 管理页实现前 |
| 远程部署验收 | TLS 终止、受信代理、Origin/CSRF、停启及未知请求取证；首版不启用 | 远程接入实现前 |

工作区、长期存储监控和自动回收另行迭代；不为预留空间提前添加无行为的 workspace/quota 占位字段。

## 7. 维护规则

1. 用户行为、契约、模型或架构边界变化时更新本文件与 CHANGELOG。
2. TASK 记录实际实现和证据，目标设计不等于已交付；任务须引用基线提交。
3. 同主题只保留一个权威正文；机器可读 API/schema 不在此复制。
4. 超过约 300 行时按领域拆分至 `spec/` 并保留索引；历史不冒充当前要求。
