# T-14 流程加固：把四天复盘中的共性成本固化为规则与工程入口

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：治理规则、工程入口与跨任务流程变更 |
| 状态 | 🟡 待验收 |
| 验收人 | 未验收（等待用户） |
| 依赖 | 无（复盘对象为 T-02～T-13） |
| AC | 流程基线：完成门禁、失败关闭、验收独立性、命令固化、收口检查 |
| 基线 | `970521e` |
| 任务分支 | `docs/zcode/gov-t-14-process-hardening` |
| 合并目标 | `develop` |
| worktree | `D:/Project/mcp-db-wt/t-14-process-hardening` |
| 任务分支提交 | 见 `TASK.md` 的 T-14 行 |
| 集成分支提交 | 见 `TASK.md` 的 T-14 行 |

## 0. 上下文包

- 权威文件：`AGENTS.md` 第 3、9 节；`project-conventions.md` 第 2、3.3、5、6 节。
- 关键契约：`TASK.md` 行格式、任务文件 6 列缺口表、提交正文 `Unverified:` trailer、worktree 约定。
- 不可变量：不改变产品行为、AC2 编号和既有验收结论；不新增治理文档。
- 入口命令：`scripts/closeout.sh`、`scripts/verify.sh`、`scripts/pgtest.sh`。
- 证据位置：本文件第 5 节与提交正文。

## 1. 范围

做什么：

- 记录 2026-11-14～11-18 的耗时、Bug、返工与重复修改，区分事实、推断和未验证项。
- 把可预防的共性问题固化为 `AGENTS.md` 与 `project-conventions.md` 的规则。
- 增加统一工程入口 `scripts/verify.sh`、真实集成入口 `scripts/pgtest.sh`、只读收口检查 `scripts/closeout.sh`。

不做什么：

- 不修改产品行为、AC2 编号、对外契约或既有验收结论。
- 不清理本任务之外的容器、分支、worktree 或未提交改动；只输出清单。
- 不引入新依赖、CI 服务、外部系统或用户级 Skill（初版曾建，复核后撤回）。
- 不提供通用冷启动 smoke 入口；入口门禁的证据命令由各任务在任务文件声明。

## 2. 验收例子

1. `scripts/closeout.sh` 只读地列出本任务 worktree 脏改动、其他 worktree 状态、未合并分支、`TASK.md` 与任务文件状态/缺口不一致、残留 `mcpdb-*` 容器，并以非零退出表示存在 FAIL。
2. `scripts/verify.sh` 的 `fmt|clippy|test|build|fast|full|real` 模式在固定 Rust 1.89 镜像内执行，使用工作区独立的 target 卷，单步有超时与耗时输出；`real` 转交 `pgtest.sh`。
3. `scripts/pgtest.sh` 在临时 PostgreSQL 16 上创建 rw/ro/provision 账号与夹具，只接受 5 个白名单测试目标，运行后在正常退出和 Ctrl-C 时都删除容器；凭证夹具测试的独立入口在脚本头部说明。
4. `AGENTS.md` 与 `project-conventions.md` 新增规则不与现有条款冲突，且能被任务文件、提交正文和收口脚本引用。
5. 复盘结论区分事实、推断和未验证项；被独立复核推翻的断言已在第 4.4 节更正。

## 3. 计划与风险

- Bug 根因检查：不适用；本任务处理流程缺陷，不修产品代码。
- 设计依据：复盘证据；`AGENTS.md` 第 3、9 节；`project-conventions.md` 第 2、3、5、6 节。
- 步骤：证据汇总 → 规则落点 → 脚本实现 → 独立复核 → 修正 → closeout 自检 → 合并与回写。
- 拆分结论：规则与脚本共用同一组根因，合并交付；Skill 因触发面过宽且不随仓库版本化而撤回。
- 并行判定：与 T-10 修复分支、T-13 UI 分支文件不重叠；本任务只碰治理文件与 `scripts/`，用独立 worktree。
- 风险：脚本只在 Windows Git Bash + Docker Desktop 验证；Linux 下 `host.docker.internal` 需要 `--add-host`，脚本已加但未实测。
- 第一性原理检查：规则必须能改变下一次行为；无法自动化的条目只做评审要求，不假装是门禁。
- 对抗性审查：脚本误报制造噪音、漏报制造虚假安全感；失败时不得自动清理未知结果。
- 奥卡姆检查：三个脚本，不再引入任务状态机、第二套证据库或用户级 Skill。

## 4. 复盘证据

### 4.1 数据来源

| 来源 | 取得的事实 |
| --- | --- |
| `git rev-list 970521e --since/--until` + `git log --name-only` | 窗口内 71 个提交、16 次合并、55 个非合并提交；其中 35 个只改 `TASK.md`/`tasks/` |
| `git show 57e89ef:src/mcp.rs`、`6a0f097` | T-05 审批/高危写入原为注释声明的 shortcut，后改为 `APPROVAL_REQUIRED` |
| `git rev-list --count develop..<branch>` | `feat/zcode/t-02-admin-console` 5 个独有提交；`feat/zcode/t-04-source-editor` 7 个；`feat/other/t-07-readonly-tests` 0 个 |
| `git worktree list` + 各 worktree `git status --porcelain` | 复盘时 3 处未收口改动 |
| `git reflog` | 无 `reset`、`rebase` 或历史改写 |
| `TASK.md` 各版本、`tasks/*.md`、`.plan/*.md` | 状态轨迹、缺口表、阶段计划与重规划记录 |
| `C:\Users\31800\.zcode\cli\db\db.sqlite`（只读） | 窗口内 3,419 次工具调用；Bash 1,463 次/10.5h；14 次 ≥9 分钟共 2.32h；3 次 `model_context_exceeded`；26 次 Edit 失败（11 次 stale read） |
| `docker ps -a` | 复盘时 `mcpdb-t10-pg` 残留约 10 小时 |
| `grep MCPDB_TEST_`、`#[ignore]` | 真实集成入口分布在 6 个测试目标、3 个 DSN 环境变量和 2 个凭证目录变量 |

### 4.2 已核实的事实

- 11-14、11-15 零提交；11-13 17:28 起会话在等语言决策，11-16 10:26 才恢复。该 turn 持续 61.2 小时，但**包含等待，不代表工作时间**。
- T-05 的 shortcut 于 16:23:38 引入，16:44:32 合入 `develop`，17:16:41 任务标 `🟢 已完成`，17:25:23 修复合入；fail-open 路径在 `develop` 存在 **40 分 51 秒**，完成声明早于修复 **8 分 10 秒**。
- T-10 的状态轨迹是 `进行中 → 🟡 待验收`，**没有发生过 `🟢 已完成 → 待验收` 回退**；`42f3f73` 前 `TASK.md` 写“进行中”，任务文件仍写“未开始”，问题是回写滞后而不是状态回滚。
- T-09/T-12 起过容器并记录 healthy，但没有覆盖 detached 重复首启；冷启动缺陷首次由 T-10 记为 `🔴 未测试`。不能据此说它们“明知缺陷仍宣称通过”。
- T-07 的 `tests/postgres_readonly.rs` 与 `tests/profile_limits.rs` 是两条不同验证路径，不是同一测试的两份实现；`t-07-readonly-tests` worktree 分支指向 `044b660`、无独有提交，属陈旧产物，不能称“并行重复实现”。
- Bun/TypeScript 基线切换有 `CHANGELOG.md` 记录，旧实现保留为 legacy v1 并被 T-03 用作迁移源；可成立的是“选型前投入了约 3.7k 行 TS 的沉没成本”，不是“纯浪费”或“私自换栈”。

### 4.3 共性根因（按证据强度排序）

| # | 根因 | 证据 | 对应措施 |
| --- | --- | --- | --- |
| R1 | 完成声明早于跨边界验证 | T-05 完成早于修复 8 分钟；T-10 冷启动缺陷在发布验收才暴露 | 入口门禁、验收独立性 |
| R2 | 延迟职责被写成放行 shortcut | `57e89ef` 的注释 + 直接执行 | 失败关闭规则 |
| R3 | 环境知识未固化 | 14 次 ≥9 分钟共 2.32h；241 次 docker；工具链 1.85/1.89 混用 | `verify.sh`、`pgtest.sh` |
| R4 | 关键决策滞后 | 61.2 小时等待；11-16 一天 50 个提交做重规划 | 决策门槛、编号不复用 |
| R5 | 并行写者与收口缺失 | 11 次 stale read；3 处脏工作区；残留容器 | `closeout.sh`、子 agent 交付提交或补丁 |
| R6 | 上下文无节制 | 3 次 context_exceeded；SPEC 读 39 次、`store.rs` 58 次 | 上下文包 |

记账提交占非合并提交的 63.6%，但这是 `AGENTS.md` 第 7 节和工程约定要求的集成分支记账方式；可优化的是**回写频率**，不能把提交占比本身当作浪费证据。

### 4.4 复核修正

初版复盘有三处断言过强，经独立复核后更正：

1. “T-10 已完成退回待验收”不成立；实际是 `进行中 → 待验收`，问题是回写滞后。
2. “T-05 合入 8 分钟后返工”不准确；fail-open 在 `develop` 存在 40 分 51 秒，任务完成声明早于修复 8 分 10 秒。
3. “T-07 存在重复实现”不成立；两个测试文件验证不同路径，worktree 分支无独有提交。

同时撤回初版 Skill：位于用户级目录、不随仓库版本化、触发词过宽且执行仓库脚本，风险大于收益；收口流程已由规则和 `closeout.sh` 承担。

## 5. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 脚本语法 | AC-2、3 | `bash -n scripts/verify.sh scripts/pgtest.sh scripts/closeout.sh scripts/check-commit-msg.sh` | Git Bash | 4 个脚本语法通过 | 无 |
| 收口脚本 | AC-1 | `bash scripts/closeout.sh` | 本仓库 + 4 个 worktree | 首次暴露 3 处脏 worktree、3 条未合并分支、1 个残留容器；修正后 0 FAIL、7 WARN | 无 |
| 固定入口 | AC-2 | `bash scripts/verify.sh fast` | Docker Desktop + rust:1.89 | 未执行 | 全部 |
| 真实集成入口 | AC-3 | `bash scripts/pgtest.sh` | Docker Desktop + postgres:16 | 未执行 | 全部 |
| 规则一致性 | AC-4 | 核对 `AGENTS.md`、`project-conventions.md`、`TASK.md` 交叉引用 | 仓库 | 新增条款与现有编号、状态定义和缺口表格式一致 | 无 |
| 独立复核 | AC-5 | 子 agent 只读复核历史与草案 | 本机 | 推翻 3 条断言，发现脚本注入、夹具权限、容器泄漏等 9 项缺陷，均已修正 | 无 |

外部写调用数：0（脚本类；运行 `pgtest.sh` 会在本机创建临时容器与测试库）。

## 6. 交付

- 提交与 `TASK.md`：见 `TASK.md` 的 T-14 行。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未测试 | `verify.sh` 全量与 release 构建 | 实施时会话正被 T-10 凭证修复与 T-13 UI 占用，未在干净工作区跑完固定镜像 | 镜像路径、target 卷和超时参数未证实；可能首次运行需调整 | 在干净提交上执行 `scripts/verify.sh full`；关闭条件：fmt/clippy/test/build/docker build 全部通过并记录耗时 | 否 |
| 🔴 未测试 | `pgtest.sh` 真实集成 | 需要独占 Docker 与临时 PG16 容器，未在本次会话执行 | 账号权限、夹具与 DSN 可能仍需按测试调整；Linux 网络路径未验证 | 执行 `scripts/pgtest.sh`；关闭条件：5 个白名单目标全部通过且容器已删除 | 否 |