# T-10 完成首版 AC2 跨边界验收与发布收口

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：覆盖首版 PostgreSQL 范围与发布门禁 |
| 状态 | 未开始 |
| 依赖 | T-03～T-09 |
| AC | AC2-01～AC2-23 的首版 PostgreSQL 范围 |
| 基线 | `57041e4` |
| 任务分支 | `test/zcode/t-10-ac2-acceptance` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | — |

## 1. 范围

- 按 `docs/PRD.md` 发布门禁和 SPEC 有序检查点汇总首版证据：
  - Docker 构建/非 root/健康检查；
  - PostgreSQL 只读查询与负向权限边界；
  - 受控写入、普通/高危审批；
  - PostgreSQL 建库、配额、登记与生命周期；
  - 本机 CLI/Web 管理面与一次性登录令牌；
  - 停启、未知结果和 Bun/TypeScript v1 迁移；
  - 审计与凭证不泄露。
- Mock E2E、真实 PostgreSQL 集成和人工验收分别记录，不能互相冒充。
- 未覆盖的 MySQL/MariaDB、SQLite、SQL Server 与跨主机远程管理明确保持未验证，交 T-11 或后续远程任务。
- 不新增产品能力或放宽 AC。

## 2. 验收例子

1. 首版每条 AC 可定位到独立证据；Mock、真实 PostgreSQL 和人工验收层级清晰。
2. PRD 第 7 节十项发布门禁全部有证据；任一缺失则不能标可发布。
3. 未覆盖驱动和远程管理在结论中明确标为后续阶段，不计入首版通过。
4. 缺口表为 `🔴 无` 或用户明确接受风险后，任务才可标完成。

## 3. 计划与风险

- Bug 根因检查：不适用；验收发现的 Bug 返回对应实现任务或建立独立修复任务。
- 设计依据：PRD 第 6～7 节、SPEC 4 端到端检查点与工程约定 3.2/3.3。
- 步骤：证据清单 → 自动化门禁 → PostgreSQL 真实集成 → 本机管理人工验收 → 迁移/停启 → 文档一致性 → 缺口关闭。
- 拆分结论：本任务只收口首版验证，避免实现提交自证完成；扩展驱动单独 T-11。
- 并行判定：各独立外部环境可并行，但证据汇总在所有首版依赖合入后进行。
- 风险：环境不足导致证据缺失；缺失必须保留为红色缺口，不降级声明。
- 第一性原理检查：声明层级不得超过独立证据层级。
- 对抗性审查：逐条寻找 Mock 冒充真实、日志冒充结果和讨论冒充验收。
- 奥卡姆检查：不新建重复测试管理体系。

## 4. 测试与证据

| 层级 | PRD 门禁 / AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 自动化套件 | 门禁 1、10；AC2-01～23 | `cargo test --locked`；clippy；`cargo build --release --locked` | Docker Linux rust 1.89 | 114 passed, 11 ignored；clippy 严格；release 通过 | 无 |
| 真实 PostgreSQL | 门禁 4、5、6、7 | 5 个测试目标 `--ignored` 顺序执行 | 隔离 PG16：rw/ro/provision 账号 + users/readonly_fixture | 9 passed：只读边界、容量/超时、建库/双角色/登记、MCP 查询与 SQL tools、真实建库 E2E | 无 |
| 容器首用 | 门禁 1、2、3、4 | `docker build`；`-p 127.0.0.1:28787:8787 -p 127.0.0.1:28788:8788`；CLI sources/profiles/routes/tokens/grants；MCP initialize + query_sql | Docker Desktop Linux VM + 挂载卷 | user 10001、health healthy；CLI 全流程成功；`list_sources` 返回 app，query 返回 2 行；未授权 source 返回 `SOURCE_NOT_GRANTED` | 见缺口：Docker Desktop detached 首启竞态 |
| 管理面 | 门禁 3、10 | `admin-login-token`；curl 登录/重放/agent token；dashboard | 同上 | 登录页 200、登录 303、dashboard 200、agent Bearer 401、令牌重放 401；页面含待批/审计且无凭证 | 浏览器人工验收待用户 |
| 审批与重启 | 门禁 6、8 | 高危 `DELETE FROM users` → `APPROVAL_REQUIRED`；重启容器 | 同上 + PG | 高危不执行且数据不变；重启后 pending 变 `RESTART_PENDING`、grants/审计保留、查询恢复 | 真实 elicitation 客户端未执行（T-06 既有非阻塞项） |
| 迁移 | 门禁 9 | `cargo test --locked --test legacy_migration` | 临时目录 legacy 夹具 | 10 passed：矩阵、无建库能力、sanitized TOML、0600 备份、两处崩溃恢复、拒绝路径 | 无 |
| 文档与一致性 | 门禁 1、9 | 检查 `docs/PRD.md`、`SPEC.md`、`TASK.md`、任务文件交叉引用 | 仓库 | 首版范围、远程/驱动未覆盖项、迁移限制一致 | 无 |

外部写调用数：真实 PG 测试创建并清理数据库/角色与 users 行；容器 smoke 写入本地状态库与 users 表；无其他外部系统。
证据位置：本任务文件与本地隔离容器记录；未记录 DSN、token、cookie 或口令。

## 5. 交付

- 提交与 `TASK.md`：待合入。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未验收 | 管理页浏览器体验 | 自动化只能验证状态码与内容，最终体验需人确认 | 门禁 3 的用户侧确认缺失 | 启动容器后执行 `docker exec mcpdb-t10-smoke mcp-db admin-login-token`，浏览器打开 `http://127.0.0.1:28788/admin` 完成登录、审批、审计查看；关闭条件：用户确认 | 否 |
| 🔴 未测试 | 真实 MCP 客户端 elicitation | 只有 fake gate 与 admin 队列证据，无真实客户端 accept/decline 会话 | 门禁 6 的 elicitation 分支未在真实客户端验证；不影响 admin 审批与状态机 | 在支持 elicitation 的客户端执行 accept/decline/cancel 并记录；关闭条件：三条真实交互证据 | 否 |
| 🔴 未测试 | Docker Desktop detached 首启 | 默认 entrypoint 直接 `-d` 启动时读取挂载凭证失败，`sleep 2` 后正常；Linux 目标环境未复现 | Docker Desktop Windows 用户可能首次启动失败 | 后续在凭证初始化加入短重试或文档说明；关闭条件：default entrypoint 连续 3 次 detached 首启成功 | 否 |
