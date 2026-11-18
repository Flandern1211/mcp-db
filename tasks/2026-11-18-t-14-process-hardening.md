# T-14 流程加固：把四天复盘中的共性成本固化为规则、脚本与 Skill

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：治理规则、工程入口与跨任务流程变更 |
| 状态 | 🟡 待验收 |
| 依赖 | 无（复盘对象为 T-02～T-13） |
| AC | AC2 流程基线：完成门禁、失败关闭、验收独立性、命令固化、收口检查 |
| 基线 | `970521e` |
| 任务分支 | `docs/zcode/gov-t-14-process-hardening` |
| 合并目标 | `develop` |
| worktree | `D:/Project/mcp-db-wt/t-14-process-hardening` |
| 任务分支提交 | `c41767b` |
| 集成分支提交 | 见 `TASK.md` 的 T-14 行 |

## 0. 上下文包

- 权威文件：`AGENTS.md` 第 3、9 节；`project-conventions.md` 第 2、3.3、5、6 节。
- 关键契约：`TASK.md` 行格式、任务文件 6 列缺口表、提交正文 `Unverified:` trailer、worktree 约定。
- 不可变量：不改变产品行为、AC2 编号和既有验收结论；不新增治理文档。
- 入口命令：`scripts/closeout.sh`、`scripts/verify.sh`、`scripts/pgtest.sh`。
- 证据位置：本文件第 5 节与提交正文。

## 1. 范围

做什么：

- 记录 2026-11-14～11-18 的耗时、Bug、返工、回滚与重复修改，并给出证据来源，避免把推断写成事实。
- 把可预防的共性问题固化为 `AGENTS.md` 与 `project-conventions.md` 的规则。
- 增加统一工程入口 `scripts/verify.sh`、真实集成入口 `scripts/pgtest.sh`、只读收口检查 `scripts/closeout.sh`。
- 在用户级 skills 目录建立 `task-closeout` Skill，调用仓库脚本执行收尾流程。

不做什么：

- 不修改产品行为、AC2 编号、对外契约或既有验收结论。
- 不清理本任务之外的容器、分支、worktree 或未提交改动；只输出清单。
- 不引入新依赖、CI 服务或外部系统。

## 2. 验收例子

1. `scripts/closeout.sh` 能只读地列出脏工作区、未合并分支、`TASK.md` 与任务文件状态/缺口不一致、残留 `mcpdb-*` 容器，并以非零退出表示存在 FAIL。
2. `scripts/verify.sh` 的 `fmt|clippy|test|build|fast|full|real` 模式在固定 Rust 1.89 镜像内执行，单步有超时与耗时输出；`real` 转交 `pgtest.sh`。
3. `scripts/pgtest.sh` 在隔离 PostgreSQL 16 上创建 rw/ro/provision 账号与夹具，运行全部 `--ignored` 集成测试，并在退出时删除容器。
4. `AGENTS.md` 与 `project-conventions.md` 新增规则可被任务文件、提交正文和收口脚本引用，且不与现有条款冲突。
5. `task-closeout` Skill 能在收尾时按脚本输出驱动缺口关闭，不新增第二套权威流程。

## 3. 计划与风险

- Bug 根因检查：不适用；本任务处理流程缺陷，不修产品代码。
- 设计依据：复盘证据；`AGENTS.md` 第 3、9 节；`project-conventions.md` 第 2、3、5、6 节。
- 步骤：证据汇总 → 规则落点 → 脚本实现 → closeout 自检 → 合并与回写。
- 拆分结论：规则、脚本、Skill 共用同一组根因，合并交付；任一单独落地都无法形成闭环。
- 并行判定：与 T-10 修复分支、T-13 UI 分支文件不重叠；本任务只碰治理文件与 `scripts/`，用独立 worktree。
- 风险：脚本只在 Windows Git Bash 与 Docker Desktop 验证；`pgtest.sh` 依赖本机 Docker 与 55432 端口。
- 第一性原理检查：规则必须能改变下一次的行为，不能只是复述原则；每条规则都要有可判定的检查或明确的评审动作。
- 对抗性审查：脚本误报会制造噪音，漏报会制造虚假安全感；`closeout.sh` 只做确定性强检查，其余给 WARN。
- 奥卡姆检查：三个脚本、一个 Skill，不再引入任务状态机或第二套证据库。

## 4. 复盘证据

### 4.1 数据来源

| 来源 | 取得的事实 |
| --- | --- |
| `git log --since=2026-11-14 --date=iso --name-status` | 71 个提交、16 次合并、35 个只改 `TASK.md`/`tasks/`（49%）；每任务起止时间 |
| `git show 6a0f097 -- src/mcp.rs` | T-05 审批/高危写入原为放行，后改为 `APPROVAL_REQUIRED` |
| `git rev-list --count develop..<branch>` | `feat/zcode/t-02-admin-console` 5 个、`feat/zcode/t-04-source-editor` 7 个独有提交 |
| `git worktree list` + 各 worktree `git status --porcelain` | 复盘时 3 处未收口改动；`t-07` 有孤立测试文件 |
| `git reflog` | 无 `reset`、`rebase` 或历史改写 |
| `TASK.md`、`tasks/*.md`、`.plan/*.md`、`.zcode/plans/*.md` | 状态回滚、缺口表、阶段计划和重规划记录 |
| `C:\Users\31800\.zcode\cli\db\db.sqlite`（只读） | 窗口内 3,419 次工具调用；Bash 1,463 次/10.5h；14 次 ≥9 分钟共 2.32h；3 次 `model_context_exceeded`；26 次 Edit 失败（11 次 stale read）；Read 次数排行 |
| `docker ps -a` | `mcpdb-t10-smoke`、`mcpdb-t10-pg` 残留 9 小时 |
| `grep MCPDB_TEST_`、`#[ignore]` | 真实集成入口分散在 5 个测试目标、3 个 DSN 环境变量 |

### 4.2 主要事实

- 11-14、11-15 零提交；11-13 17:28 至 11-16 10:26 的会话在等语言决策，其中一个 turn 持续 61.2 小时。
- T-05 17:16 标完成并合入，17:24 补 `fix(T-05)` 失败关闭，17:25 重新收口。
- T-10 01:24 标完成，01:25 降为 `🟡 待验收`；冷启动竞态到 T-10 才发现。
- `c3d6fd4` 记录两处真实库测试不可重复：断言依赖夹具行数、测试间共享状态。
- `src/lib.rs` 被 9 个任务修改，`main.rs` 8 个，`transport/http.rs` 与 `state/store.rs` 各 7 个。
- 手写 `docker run ... cargo ...` 有 14 次撞 10 分钟上限，合计 2.32h，占 Bash 执行时间 22%。

### 4.3 共性根因

| # | 根因 | 证据 | 对应措施 |
| --- | --- | --- | --- |
| R1 | 完成判定用当前层级测试替代最终入口可用 | T-05、T-10 状态回滚；冷启动缺陷后置 | 入口门禁、验收独立性、`closeout.sh` |
| R2 | 延迟职责默认放行 | `6a0f097` 前的 shortcut 直执行 | 失败关闭规则 |
| R3 | 环境知识未固化 | 14 次超时、241 次 docker、1.85/1.89 混用 | `verify.sh`、`pgtest.sh` |
| R4 | 关键决策滞后、任务号复用 | 61.2h 挂起；两条废弃分支 | 决策门槛、编号只退休不复用 |
| R5 | 并行写者与收口缺失 | 11 次 stale read；T-07 双份实现；残留容器 | 热文件串行、`closeout.sh`、Skill |
| R6 | 上下文无节制 | 3 次 context_exceeded；SPEC 读 39 次、`store.rs` 58 次 | 任务上下文包 |

## 5. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 脚本语法 | AC-2、3 | `bash -n scripts/verify.sh scripts/pgtest.sh scripts/closeout.sh scripts/check-commit-msg.sh` | Git Bash | 4 个脚本语法通过 | 无 |
| 收口脚本 | AC-1 | `bash scripts/closeout.sh` | 本仓库 + 4 个 worktree | 首次运行暴露 3 处脏 worktree、3 条未合并分支、1 个残留容器；修复解析与 locale 后仅剩“当前任务未提交”1 项 FAIL；提交后复跑 0 FAIL、8 WARN | 无 |
| 固定入口 | AC-2 | `bash scripts/verify.sh fast` | Docker Desktop + rust:1.89 | 未执行 | 全部 |
| 真实集成入口 | AC-3 | `bash scripts/pgtest.sh` | Docker Desktop + postgres:16 | 未执行 | 全部 |
| 规则一致性 | AC-4 | 核对 `AGENTS.md`、`project-conventions.md`、`TASK.md` 交叉引用 | 仓库 | 新增条款与现有编号、状态定义和缺口表格式一致 | 无 |

外部写调用数：0（脚本类，运行 `pgtest.sh` 会在本机创建临时容器与测试库）。

## 6. 交付

- 提交与 `TASK.md`：待填。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未测试 | `verify.sh` 全量与 release 构建 | 实施时会话正被 T-10 凭证修复与 T-13 UI 占用，未在干净工作区跑完固定镜像 | 脚本参数与缓存卷行为未证实；可能首次运行需调整 | 在干净提交上执行 `scripts/verify.sh full`；关闭条件：fmt/clippy/test/build 全部通过并记录耗时 | 否 |
| 🔴 未测试 | `pgtest.sh` 真实集成 | 需要独占 55432 端口与临时 PG16 容器，未在本次会话执行 | 账号、夹具或 DSN 可能需按测试实际断言调整 | 执行 `scripts/pgtest.sh`；关闭条件：5 个 `--ignored` 目标全部通过且容器已删除 | 否 |