## Context

用户已确认：核心服务改用 **Rust**；首版目标是 **个人开发者**，MVP 包含 **只读查询、受控写入、受控建库**；**首版普通能力仅 PostgreSQL**；交付形态为 **Docker 服务**；管理入口为 **CLI + Web 管理页**；远程管理面**不进入首版**，管理页走 **本机端口 + CLI 生成的一次性登录令牌**。

现有 `SPEC.md` 是系统基线，但缺少产品意图、用户、场景优先级和成功指标；`README.md` 仍是旧 v1。当前 `feat/zcode/t-02-state-identity` 分支尚无独有提交，原 T-02 的 Bun/TypeScript 计划需重规划，而不是在旧计划上继续写 Rust 代码。

本计划先完成“PRD + Rust 基线”这一治理层，再按新的 MVP 范围重排任务；不开始产品代码实现。

## 1. 治理分支与现状保护

1. 先核对 `git status --short --branch`、已暂存/未暂存/未跟踪文件。
2. 当前工作区若无 T-02 独有改动：从 `develop` 创建 `docs/zcode/gov-rust-rebaseline`；暂不删除空 T-02 分支，等新基线合入后再决定是否重建。
3. 若有未提交改动：不覆盖、不 reset；先纳入独立 legacy 分支或由用户决定放弃，绝不计入 Rust 任务。
4. 只暂存本治理范围文件；不触碰未跟踪的 `.zcode/`。

## 2. 新增 `docs/PRD.md`

产品层只写以下内容，不复制 SPEC 的表结构、AC 编号、crate 或 handler 细节：

- 文档状态与权威边界：PRD 负责产品意图/用户/场景/范围/成功指标；SPEC 负责系统设计与 AC；TASK 负责状态和证据。
- 产品愿景：给 AI agent 提供统一、可审计、精确授权的多 SQL 数据源 MCP 入口。
- 目标用户：个人开发者/单管理员；多个 agent/token；数据库 DBA 只提供原生权限与备份能力。
- 核心场景，按用户旅程：
  1. 通过 Docker 启动服务并登记 PostgreSQL source/profile。
  2. 签发 token，按 source → tool 授予查询、写入、建库能力。
  3. agent 发现 source/tool，执行只读查询。
  4. 非生产受控写入按 grant 策略与审批执行。
  5. 高危操作与建库逐次由管理员批准。
  6. 管理员审批、查看审计、归档/恢复 source。
- MVP 范围：PostgreSQL 普通能力、只读、受控写入、受控建库、CLI + Web 管理页、本机端口管理、Docker 交付。
- 明确延后：MySQL/MariaDB、SQLite、SQL Server 的普通能力；远程管理；多管理员/多租户；自动删库；跨驱动存储配额等。
- 产品原则与安全承诺边界：默认拒绝、可见不等于授权、数据库原生权限是最终边界、高危不可 direct、未知结果不重试、截断不等于扫描限制、归档不释放物理存储。
- 成功指标（不编造使用数字）：
  - 未授权 source/tool 调用为 0。
  - 每个已发布驱动只读边界负向验证通过。
  - 高危/建库审批均逐次 admin 决策，重复消费为 0。
  - 重启后旧批准/外部写重放为 0。
  - 活动配置切换后无新旧混合授权。
  - 从空环境到完成第一次授权只读查询有明确步骤和可判定结果。
- 产品级验收/首发门禁：安装、首次配置、token 签发、管理页登录、一次查询、一次审批、一次建库、重启、迁移、审计查看。
- 未决项：默认资源限制数值、审计保留周期、建库命名与账号策略、Web 管理页具体信息架构。明确这些不阻塞 PRD 批准，但必须在对应实现任务前确定。

## 3. 更新系统与技术基线

- `SPEC.md`：
  - 顶部新增 PRD 链接和权威边界。
  - 架构改为 Rust 单进程服务；不提前锁死具体 crate，保留 `/mcp`、`/admin`、状态库路径。
  - 支持矩阵改为首版仅 PostgreSQL 普通能力；其余驱动列为后续阶段，未支持驱动必须明确拒绝。
  - 远程管理移至后续阶段；首版管理页只允许本机端口 + CLI 生成的一次性登录令牌。
  - 迁移章节明确“旧实现”指 Bun/TypeScript v1，目标是 Rust AC2；旧源码/二进制保留为迁移夹具直到 T-03 完成。
  - AC2 保留编号和产品语义；把与首版范围冲突的“四驱动首版”表述改成“首版 PostgreSQL，其余按阶段扩展”。
- `CHANGELOG.md`：新增 2026-11-16 条目，记录技术栈改为 Rust、首版范围收敛、交付形态和远程管理延后；不改写 2026-11-11 历史。
- `project-conventions.md`：
  - 工程入口改为 Rust：`cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test`、`cargo build --release`；Docker 构建/冒烟入口独立。
  - 保留 Bun 命令为 legacy v1 迁移夹具入口，不再作为正式开发入口。
  - 记录 `Cargo.lock` 提交、toolchain 锁定、支持目标（Linux 容器 + 后续原生平台）。
  - 测试隔离、真实数据库集成、凭据和 Git 规则沿用。
- 根目录工程脚手架：
  - 新增 `Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`src/main.rs` 最小可运行骨架、`Dockerfile`、`.dockerignore`。
  - 暂不迁移业务逻辑；旧 `package.json`/`src/*.ts`/`tests/*.ts` 保留为 legacy v1，不删除。
  - 若首版暂不引入 MCP/DB crate，骨架只包含启动、健康检查和配置占位；依赖在 T-02/T-04/T-05 按 spike 结果添加。

## 4. 重排任务 DAG

保留任务编号和产品结果，更新任务文件内容与基线引用：

- **T-02**：改为“建立 Rust 工程与 AC2 状态/身份基础”。包含 Cargo 工程、模块边界、状态 schema、token/grants/profile/route 基础模型、领域错误、Docker 本地运行骨架；AC 仍为 AC2-01/09/11/22 的基础。
- **T-03**：迁移源明确为 Bun/TypeScript v1 TOML + SQLite → Rust 状态模型；依赖 T-02。
- **T-04**：精确 grants、PostgreSQL profile 路由、工具发现、本机会话刷新；依赖 T-02。
- **T-05**：PostgreSQL SQL parser/policy spike 优先；实现 `query_sql`、`mutate_sql`、`execute_ddl`；其他方言 parser 作为扩展点，不进入首版验收。
- **T-06**：审批状态机、摘要、一次性消费、审计；依赖 T-02/T-04/T-05。
- **T-07**：PostgreSQL profile 资源限制与只读边界；MySQL/SQLite/SQL Server 负向矩阵移入后续驱动扩展任务或在 T-10 标注未覆盖。
- **T-08**：PostgreSQL 建库、配额、新 source 登记、归档/恢复；依赖 T-04～T-07。
- **T-09**：改为“首版本机 Web 管理页与 CLI 管理面”；本机端口 + 一次性登录令牌；远程 TLS/代理/Origin/CSRF 延后到后续任务，不进入首版发布门禁。
- **T-10**：首版发布门禁只覆盖 PostgreSQL 的 AC2 子集和已确认产品场景；未覆盖驱动、远程管理明确列为后续阶段，不用 Mock 冒充真实验收。
- 新增一个后续驱动扩展任务（T-11）或在 T-10 后设“待规划”条目，避免把 MySQL/SQLite/SQL Server 悄悄算进首版。
- 所有任务基线引用从 `60aeb32` 改为 Rust 基线治理提交；PRD 作为产品权威链接。

## 5. 验证

文档阶段：

- 检查 `docs/PRD.md`、`SPEC.md`、`CHANGELOG.md`、`project-conventions.md`、`TASK.md` 和任务文件的交叉引用。
- `git diff --check`；确认没有真实凭据、客户数据或内部地址。

工程骨架阶段：

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo build --release`
- `docker build -t mcp-db:dev .` 和容器启动/健康检查冒烟
- 明确记录当前没有业务实现，T-02 不因骨架存在而标完成。

提交与状态：

- 治理变更单独提交，正文写 AC/范围、实际命令、环境、外部写次数和 `Unverified`。
- 合入 `develop` 后回写 `TASK.md`；T-02 在 Rust 骨架和状态基础验收前保持进行中或未开始，不虚报完成。
- 不推送、不打标签、不发布。

## 明确不做

- 不在本阶段删除 TypeScript v1 或重写业务逻辑。
- 不实现远程管理面、TLS 代理、MySQL/SQLite/SQL Server 普通能力。
- 不把 MCP SDK、SQL parser、数据库驱动代码在 spike 前写入正式实现。
- 不创建无行为的 workspace/多租户/存储配额占位字段。