# T-14 流程加固：把四天复盘中的共性成本固化为规则与工程入口

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：治理规则、工程入口与跨任务流程变更 |
| 状态 | 🟢 已完成 |
| 验收人 | 非原实现者独立复核与复验（2026-11-22，两轮）；原复跑：主会话（2026-11-18，未参与实现） |
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
| 固定入口 | AC-2 | `bash scripts/verify.sh full` | Docker Desktop + rust:1.89-slim-bookworm | 通过：fmt 4s、clippy 156s、test 114 passed/11 ignored 162s、release build 352s、docker build 通过；整组 682s，0 warning | 无 |
| 真实集成入口 | AC-3 | `bash scripts/pgtest.sh` | Docker Desktop + postgres:16（临时容器，端口 32768） | 通过：postgres_metadata 2、postgres_readonly 2、profile_limits 1、provision_postgres 1、http_mcp 3，共 9 passed；容器已删除 | 无 |
| 规则一致性 | AC-4 | 核对 `AGENTS.md`、`project-conventions.md`、`TASK.md` 交叉引用 | 仓库 | 新增条款与现有编号、状态定义和缺口表格式一致 | 无 |
| 独立复核 | AC-5 | 子 agent 只读复核历史与草案 | 本机 | 推翻 3 条断言，发现脚本注入、夹具权限、容器泄漏等 9 项缺陷，均已修正 | 无 |

外部写调用数：0（脚本类；运行 `pgtest.sh` 会在本机创建临时容器与测试库）。

## 6. 交付

- 实现提交见 `TASK.md`：`f30d9b4`、`f25869a`、`abb3e61`。已接受风险：无。
- 历史验收复跑（2026-11-18）：`bash scripts/closeout.sh` → 0 FAIL、9 WARN；`bash scripts/verify.sh fast` → OK；`bash scripts/pgtest.sh` → 9 passed、容器已删除。这些成功命令不检验超时语义。
- 2026-11-22 第一轮独立复核（Windows Git Bash + Docker Desktop）：5 个入口脚本逐一 `bash -n` 均通过；回填前 `GIT_OPTIONAL_LOCKS=0 bash scripts/closeout.sh` 退出 0，0 FAIL、10 WARN。发现 [`scripts/verify.sh`](../scripts/verify.sh) 只给整组计时，组内步骤只记耗时，后续 `docker build` 也没有超时，不符合 AC-2。
- 同日修复：fmt、clippy、test、release build、`docker build` 各自使用 `MCPDB_STEP_TIMEOUT`（默认 900 秒）；容器整组仍有 `MCPDB_GROUP_TIMEOUT` 上限（默认 1800 秒，未设置时兼容 `MCPDB_TIMEOUT`）。0、显式空值和非法值在调用 Docker 前拒绝；`--help` 无需 Docker。失败时只查询并报告本次调用遗留的容器，不主动清理。
- 修复后独立复验（AC-2）：`bash tests/verify_timeouts.test.sh` 退出 0，24/24 通过。隔离替身验证单步超时后不运行后续步骤、镜像构建超时退出非零、退出 137 单独报告而不误报为超时、残留只读查询。`bash scripts/verify.sh full` 在真实 Docker/Rust 1.89 环境退出 0：fmt、clippy、test、release build、镜像构建全通过；Rust 139 通过、0 失败、11 个外部集成测试按默认配置忽略。未故意让真实镜像构建超时或制造 OOM；`pgtest.sh` 未改动，AC-3 沿用历史实测。私有本机证据：`.plan/ego-t13-t18-20261122/t14-timeouts-final.log`、`verify-full-final.log`。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |

## 7. 详细证据链

每条措施只回答四个问题：证据是什么、为什么是共性问题、具体改了什么、怎么知道它防住了。

### 7.1 失败关闭

- 证据：`57e89ef:src/mcp.rs` 的 `// shortcut: approval policy handoff ... is owned by T-06.` 后直接调用 executor；16:44:32 合入，17:16:41 任务标完成，17:24:51 修复，17:25:23 合入。
- 判断：注释已经声明“以后再做”，运行时却“现在就执行”。问题不是漏写，而是把延迟职责实现成了放行。
- 方案：`AGENTS.md` 第 3.8 条限定到权限、审批、写入路径；这些分支必须拒绝并带反例测试。只读占位允许保留，但必须写明返回值和验证层级。
- 验证：`6a0f097` 的 `http_mcp` 测试断言 executor 调用数为 0；后续同类分支复用该断言。

### 7.2 入口门禁

- 证据：T-10 任务文件第 71 行记录默认 ENTRYPOINT `-d` 首启读取挂载凭证失败，`sleep 2` 后正常；T-09/T-12 只记录“容器启动 healthy”，没有 detached 重复首启。
- 判断：每个任务只验证自己那一层；启动、挂载、凭证的交叉路径没有归属，直到发布验收才暴露。
- 方案：涉及启动、配置、传输、凭证、镜像的改动，`🟢` 前必须从新建容器/卷/目录走一次真实入口，命令写进任务文件；否则最高 `🟡`。
- 验证：各任务在任务文件声明入口命令；T-14 不提供通用 smoke，避免无人执行的假门禁。

### 7.3 验收独立性

- 证据：T-10 任务文件第 56 行写“由主 agent 代替用户完成”；T-06 因真实 elicitation 未测保持 `🟡`。
- 判断：“我跑过了”不等于“用户接受了”；实现者自写的脚本也不能作为独立证据。
- 方案：由用户确认，或由未参与实现的会话/agent 复现；没有第二方时任务最高 `🟡`。
- 验证：任务文件新增“验收人”字段；T-14 保持 `🟡` 直到用户确认。

### 7.4 统一验证入口

- 证据：窗口内 Bash 1,463 次、10.5 小时；14 次单次 ≥9 分钟共 2.32 小时；241 次 `docker run/exec`；早期 `rust:1.85` 与 `rust:1.89` 混用。
- 判断：手写长命令会同时引入拼写错误、超时和工具链漂移；每次重新拼命令，等于每次重新支付环境学习成本。
- 方案：`scripts/verify.sh` 固定镜像与工具链，按 worktree 使用独立 target 卷，单步 900 秒超时并打印耗时，Windows 路径用 `pwd -W`，启动前检查 docker daemon 与 `timeout`。
- 验证：见第 5 节实际执行结果；未通过前不标完成。

### 7.5 真实集成入口

- 证据：6 个 `#[ignore]` 目标；需要 3 个 DSN 环境变量和 2 个凭证目录变量；`postgres_metadata.rs` 要求 tester 对 `users` 做 INSERT/DELETE；`c3d6fd4` 记录两处真实库测试不可重复。
- 判断：夹具靠记忆重建，就会出现“上次能跑、这次不行”；共享可变夹具让结果依赖顺序和历史状态。
- 方案：`scripts/pgtest.sh` 起临时 PG16、动态端口、白名单目标、`SET ROLE tester` 建夹具、信号退出也删除容器；凭证测试单独说明。
- 验证：见第 5 节；脚本本身也经过独立复核，修正了命令注入和夹具权限两处缺陷。

### 7.6 收口检查

- 证据：复盘时 3 处脏工作区、2 个残留容器；`TASK.md` 被改 28 次；T-10 出现过 `TASK.md` 进行中、任务文件未开始的不一致。
- 判断：收口靠记忆，最先失效的就是记忆；但要求所有 worktree 干净会阻塞并行任务，也会诱导清理他人工作。
- 方案：`scripts/closeout.sh` 只把**本任务 worktree** 的脏改动判 FAIL；其他 worktree、未合并分支、残留容器只 WARN；同时检查 `TASK.md` 与任务文件的状态/缺口数、HEAD 的 `Unverified:`。
- 验证：收尾运行时 0 FAIL；WARN 为其他任务的脏 worktree、未合并分支和残留容器，清单见第 8 节。脚本显式 `LC_ALL=C`，避免交互 shell 的 ugrep alias 改变多字节匹配行为。

### 7.7 任务编号与决策门槛

- 证据：语言决策等待 61.2 小时；11-16 一天 50 个提交重规划；`feat/zcode/t-02-admin-console`（5 个独有提交）和 `feat/zcode/t-04-source-editor`（7 个）与现行 T-02/T-04 范围不符。
- 判断：编号保留、范围整体改写，会让分支名和任务表指向不同东西；旧提交并非垃圾，但失去了可追溯语义。
- 方案：编号只退休不复用，旧编号标“已废弃”并写原因；阻塞性决策未定时不得开始编码，允许先做 spike。
- 验证：评审项；靠分支名、任务表和 `CHANGELOG` 对照，无脚本。

### 7.8 夹具顺序无关

- 证据：`c3d6fd4` 正文记录“users 行数断言改为与夹具规模无关；sql_tools 测试前后重置夹具”；`postgres_metadata.rs` 只在末尾删除 `id=99`。
- 判断：中途失败会留下残留，重跑主键冲突；断言依赖历史行数时，单独跑和串行跑结果不同。
- 方案：每个用例前后重置自己写入的数据；断言不依赖历史行数；任一用例可单独运行。
- 验证：`pgtest.sh` 每次全新数据库兜住跨运行污染；用例内部的顺序依赖仍需测试作者遵守。

### 7.9 任务上下文包

- 证据：3 次 `model_context_exceeded`（57 分钟）；`SPEC.md` 读 39 次、`store.rs` 58 次、`mcp.rs` 52 次、`http_mcp.rs` 43 次；23 次子 agent 调用共 1.24 小时。
- 判断：任务文件只有范围，没有“先读哪五个东西”，每个任务都从重读大文件开始。
- 方案：完整档任务文件写五行上下文包：权威文件、关键契约、不可变量、入口命令、证据位置。
- 验证：评审项；T-14 自身已按该格式写第 0 节。

### 7.10 撤回的 Skill

- 证据：初版建在用户级目录，不随仓库版本化；触发词覆盖“收尾/提交/验收”；执行的是仓库内脚本。
- 判断：收益只是把 `closeout.sh` 再包一层，风险是宽触发面加运行当前仓库代码。
- 方案：删除；流程由规则和脚本承担。若将来重建，必须入仓、限定仓库来源，并把顺序改成“提交后检查”。

### 7.11 未纳入规则的现象

- 记账提交占非合并提交 63.6%，但这是约定要求，不作为浪费证据；可优化的是回写频率。
- `lib.rs`、`main.rs` 被多个任务修改是装配入口的结构使然；要防的是同一波次并行改同一文件，已由 6.9.2 覆盖，不再新增静态热文件清单。

## 8. 遗留清单（截至 2026-11-18 11:00，处置前需重新核对）

| 项 | 现状 | 建议 |
| --- | --- | --- |
| `fix/zcode/t-10-credential-startup` | 最新提交 `144a1c3 wip: 空文件重试（基于错误前提，未合入）`，未合并 | 由该任务的执行者确认前提是否成立；不成立则关闭分支并重开 T-10 缺口，不沿用旧结论 |
| `mcpdb-db-wt/t-13-admin-ui` | 2 个文件、约 550 行未提交；任务“进行中” | 提交到任务分支后再验收；不要直接删除 |
| `mcpdb-db-wt/t-07-readonly-tests` | 未跟踪的 135 行 `tests/postgres_readonly.rs`，与已提交的 206 行版本不同；分支 0 独有提交 | 人工 diff 后删除 worktree 和分支 |
| `feat/zcode/t-02-admin-console` | 5 个 Bun 时代独有提交，名称与现 T-02 不符 | 先导出 patch 再删除 |
| `feat/zcode/t-04-source-editor` | 7 个 Bun 时代独有提交，名称与现 T-04 不符 | 先导出 patch 再删除 |
| `mcpdb-t10-pg` | 容器已运行约 12 小时 | 确认无测试占用后 `docker rm -f mcpdb-t10-pg` |
| `mcpdb-t13-smoke` | 容器已运行约 1 小时，healthy | T-13 验收结束后由执行者删除；不要与 T-10 的容器混用 |
| `.zcode/` | 主工作区未跟踪 | 决定是否加入 `.gitignore`；不要提交 |