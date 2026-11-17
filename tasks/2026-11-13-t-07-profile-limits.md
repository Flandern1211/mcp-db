# T-07 实现 PostgreSQL profile 限制与只读边界

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：真实数据库权限边界与资源限制 |
| 状态 | 未开始 |
| 依赖 | T-04、T-05 |
| AC | AC2-02、AC2-08、AC2-11 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-07-profile-limits` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | — |

## 1. 范围

- 开工前确定 pool/timeout/rate-limit 默认值与重启语义。
- 实现每 profile 连接上限、statement timeout、token 请求速率和“不支持即拒绝配置”。
- PostgreSQL query profile 绕过 MCP 分类直测 INSERT/UPDATE/DELETE/DDL，复核夹具不变与正常查询恢复。
- 不实现建库（T-08）、管理面（T-09）或其他驱动（T-11）。

## 2. 验收例子

1. PostgreSQL 并发连接、语句超时和 token 速率达到配置边界时稳定拒绝。
2. 不支持的硬限制配置明确失败，不静默忽略。
3. PostgreSQL query profile 的写/DDL 全部由数据库权限拒绝，数据/schema 不变，随后正常查询仍成功。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 3.2、4、6。
- 步骤：限制设计 → pool/driver 实现 → Mock 并发/时钟测试 → 独立 PostgreSQL 真实集成。
- 拆分结论：真实权限证据有独立环境与失败边界；其他驱动归 T-11。
- 并行判定：T-05 合入后实施；与 T-06 若文件不重叠可并行。
- 风险：把应用层分类误当只读；验收必须绕过 MCP 分类。
- 第一性原理检查：数据库身份是最终能力边界。
- 对抗性审查：覆盖 DML、DDL、超时后恢复和夹具不变。
- 奥卡姆检查：不承诺扫描量或跨驱动存储限额。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元/模块集成 | AC2-08、AC2-11 | `cargo test`；限流窗口、SQLite 硬限制拒绝、profile capacity admission | Docker Linux + 临时 SQLite | 默认 11 tests passed；MCP 限流按 token 跨 session 共享，重启语义为内存窗口重置 | 无 |
| PostgreSQL 真实集成 | AC2-02、AC2-08 | `cargo test --test postgres_readonly --test profile_limits -- --ignored` | 本地 Docker PostgreSQL 16；明确只读 fixture/角色 | 3 passed：账户直连与 query profile 会话两条路径各跑 INSERT/UPDATE/DELETE/CREATE TABLE，均被拒（42501 或只读事务 25006），夹具行与 public schema 不变且查询继续成功；单 profile 连接额度共享 SQL/metadata；50ms statement timeout 生效且之后查询可用 | 无 |
| MCP 生命周期 | AC2-08、AC2-22 | `cargo test --test http_mcp mcp_cancellation` | Mock executor + 临时 SQLite | 1 passed：query 取消释放 future/permit；已发起写入取消记录 `direct_unknown`，不误记失败、不重试 | 无 |
| 构建与静态检查 | 工程基线 | `cargo fmt --check`；clippy driver check；`cargo build --release --locked`；`docker build`；容器健康检查 | Docker Desktop Linux | 全量测试 57 passed, 9 ignored；clippy 严格检查通过；镜像构建与非 root 健康检查通过 | 无 |

测试/验收分离：本任务已由隔离容器、专用只读账号和无并发写的 fixture 自动验证；未声称人工客户端体验验收。token rate limit 和连接/timeout 默认值已写入 SPEC 与 CHANGELOG。

## 5. 交付

- 提交与 `TASK.md`：待合入。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
