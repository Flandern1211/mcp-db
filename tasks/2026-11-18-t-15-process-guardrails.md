# T-15 流程补强：把 2026-11-18 的返工与事故固化为门禁

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：治理规则、工程入口与跨任务流程变更 |
| 状态 | 🟢 已完成 |
| 验收人 | 独立验收会话（2026-11-22；未编写本次脚本修复），按用户要求复验通过后回填 |
| 依赖 | T-14（统一入口与收口检查） |
| AC | 流程基线：UI 基线、诊断顺序、资源预算、文件归属、声明证据、删除授权 |
| 基线 | `develop`（T-14 合并后） |
| 任务分支 | `docs/zcode/t-15-process-guardrails` |
| 合并目标 | `develop` |
| worktree | `D:/Project/mcp-db`（主工作区） |
| 任务分支提交 | 见 `TASK.md` 的 T-15 行 |
| 集成分支提交 | 见 `TASK.md` 的 T-15 行 |

## 0. 上下文包

- 权威文件：`AGENTS.md` 第 3、4、5、9 节；`project-conventions.md` 第 2、3 节。
- 复盘证据：`tasks/2026-11-18-t-14-process-hardening.md` 第 4 节；2026-11-18 当日 25 个提交、5 次 merge、两次管理台返工、三次 Origin 重建、一次磁盘耗尽。
- 不可变量：不改产品行为、AC2 编号、对外契约和既有验收结论；不新增治理文档；不自动删除卷、镜像或 worktree。
- 入口命令：`scripts/disk-guard.sh`、`scripts/verify.sh`、`scripts/closeout.sh`。
- 证据位置：本文件第 5 节与提交正文。

## 1. 范围

做什么：

- 在 `AGENTS.md` 固化 4 条行为边界：UI 先锁定基线、声明必须有证据、子 agent 文件归属、破坏性操作先审批。
- 在 `project-conventions.md` 固化 2 条工程规则：先诊断后构建、构建资源预算与清理边界。
- 新增 `scripts/disk-guard.sh`：默认只读报告，`--check` 在磁盘过低时失败，`--prune` 只清停止的 `mcpdb-*` 容器，`--prune-cache` 单独确认共享构建缓存；不碰卷、带标签镜像和其他项目容器。
- `scripts/verify.sh` 在运行前调用磁盘检查；Docker 不可达时给出 WSL 代理排查提示。

不做什么：

- 不修改产品代码、测试、依赖或数据库 schema。
- 不清理任何现有镜像、卷、容器或 worktree；`--prune` 只由使用者显式执行。
- 不做用户级 Skill；T-14 已因不可版本化和触发过宽撤回，本任务不重复。
- 不引入新的 CI 服务或常驻监控。

## 2. 验收例子

1. `AGENTS.md` 明确：UI/交互任务在编码前记录基线 commit/URL/截图并经用户书面确认；未确认不得以“测试通过”宣称完成。
2. `AGENTS.md` 明确：删除镜像、卷、target、容器、worktree 前必须列出对象、大小、影响和可恢复性并取得明确同意；反问、沉默和旧授权都不算同意。
3. `AGENTS.md` 明确：涉及“用户反馈/已修复/已验证”的声明必须附消息 ref、命令输出或测试证据；无证据只能标未验证，不得用推断补位。
4. `project-conventions.md` 明确：行为修复先给日志/请求复现和假设，一次构建只验证一个已确认假设；构建前检查磁盘，收口时按保留策略清理。
5. `scripts/disk-guard.sh` 无参数时只读列出主机磁盘、Docker 占用、`mcpdb-*` 镜像/卷和 worktree target；`--check` 在可用空间低于阈值时非零退出；`--prune` 只删除停止的 `mcpdb-*` 容器，`--prune-cache` 单独清理共享构建缓存；不删除卷、带标签镜像、源码或其他项目的容器。
6. `scripts/verify.sh` 在任何模式前调用 `disk-guard.sh --check`，失败时给出可执行提示；`fast` 模式仍完成 fmt/clippy/test。
7. 任务文件记录实际命令、环境、预期和结果；脚本改动通过 `bash -n` 与实际运行验证。

## 3. 计划与风险

- Bug 根因检查：不适用；本任务处理流程缺陷，不修产品代码。
- 设计依据：2026-11-18 复盘（两次 UI 返工、Origin 重建、Docker/磁盘事故、并发编辑、无据声明、未授权删除）。
- 步骤：规则落点 → `disk-guard.sh` → `verify.sh` 预检 → `bash -n` 与实跑 → 独立复核 → `closeout.sh` → 合并与回写。
- 拆分结论：规则与脚本共用同一组根因，合并交付；Skill 不做。
- 并行判定：只碰 `AGENTS.md`、`project-conventions.md`、`scripts/`、`TASK.md` 和本文件；与 T-11/T-13 的源码和测试文件不重叠。
- 风险：磁盘阈值在不同机器上差异大，默认值只做灾难保护，不替代人工判断；`--prune` 仍是破坏性操作，必须在输出中明确列出将删除的对象。
- 第一性原理检查：规则必须改变下一次行为；能被脚本门禁的进入脚本，不能门禁的写成评审要求，不假装自动验证。
- 对抗性审查：`disk-guard.sh` 误报会阻塞正常构建，漏报会制造虚假安全感；阈值可用环境变量覆盖并在输出中打印实际值与来源。
- 奥卡姆检查：只加一个脚本和两条预检，不引入状态机、守护进程或第二套证据库。

## 4. 复盘证据（摘要）

- 管理台两次返工：T-09 最小管理面 → T-13 首版 +574 行被否定 → 按 v1 重做，`26ce842` 共 6897 insertions。
- Origin null：日志已给出 `origin="null"`，仍连续重建 `t13-ui2`/`ui3`/`ui4`/`ui5`，真正修复是判断分支加登录 challenge。
- 资源：41GB `docker_data.vhdx`、15.16GB 构建缓存、5 个 target 卷约 18GB、主 target 12GB；D 盘仅剩 27MB，Docker 只读。
- 职责：T-13 子 agent 未交付时主会话直接改 `admin.rs`，用户当场指出。
- 声明：曾声称“修了三个问题”但不属实；T-10 Docker 首启缺陷基于错误前提，WIP 未合入。
- 删除：33GB 构建产物在未取得明确同意时被删除；已删项可重建，未动源码、`.git`、状态库、凭证和用户容器。

## 5. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 语法 | 5、6 | `bash -n scripts/disk-guard.sh scripts/verify.sh` | Git Bash | 通过（无输出） | — |
| 只读报告 | 5 | `bash scripts/disk-guard.sh` | Git Bash + Docker Desktop 4.97 | 通过；输出 10.1 GiB 可用、`mcp-db:t13-ui4`、4 个 `mcpdb-*` 卷、1 个 worktree target | — |
| 阈值检查 | 5、6 | `bash scripts/disk-guard.sh --check` | 同上 | 通过；`OK: host free space 10.1 GiB`，退出 0 | — |
| 清理边界 | 5 | `printf 'n\n' \| bash scripts/disk-guard.sh --prune`；`--prune-cache` | 同上 | `--prune` 未发现停止的 `mcpdb-*` 容器，退出 0；`--prune-cache` 打印共享警告后在确认处中止，退出 1；未删除任何对象 | — |
| 预检集成 | 6 | `bash scripts/verify.sh fast` | 同上，Rust 1.89 容器 | 通过；`OK fast (423s)`，退出 0；fmt/clippy/test 全部通过 | — |
| 规则一致性 | 1～4 | 核对 `AGENTS.md`、`project-conventions.md` 编号与交叉引用 | 仓库 | 通过；UI 基线见 §3.1，删除授权见 §4.6，文件归属见 §5.4，声明证据见 §9.11，诊断/资源见 §2.1/§2.2 | — |

**2026-11-22 第一轮独立复核（Windows Git Bash + Docker Desktop）**：默认阈值通过，强制提高阈值则退出 1；发现旧提示写错了 `--prune` 的范围、`MCPDB_PRUNE_CONFIRM=1` 可跳过确认，以及只读报告在 75 秒内未完成 target 扫描。以上为修复前结果。

**同日修复后独立复验**：

- `--prune` 仅列出已创建未启动或已退出、名为 `mcpdb-*` 且有 `mcpdb.project=mcp-db` 标签的容器；列出完整 ID、Docker SIZE、影响和可恢复性。未标记的旧容器和其他项目标签默认排除。确认后按已列完整 ID 执行非强制删除，不带卷删除参数。`--prune-cache` 单独显示共享构建缓存的聚合占用与可回收量，清理范围限于 Docker 默认可清理缓存。
- 两个入口均需输入本次清单生成的确认码；遗留环境变量不能绕过确认，确认码也不替代操作者取得明确授权。`bash tests/disk_guard.test.sh` 退出 0，191 项断言通过：无有效确认时不发删除调用；替身收到有效确认后，各只发对应的精确命令。未执行真实容器或缓存清理。
- 真实只读报告在 35 秒内退出 0，主机、Docker 和工作区三节均返回，一个慢 target 被标注超时；用 `MCPDB_DU_TIMEOUT=120` 重跑后 5 个 target 均返回大小。`--check` 以 30.9 GiB 可用空间、5 GiB 默认阈值通过；强制提高阈值后，`scripts/verify.sh fast` 在执行 Rust 步骤前退出 1，提示准确区分两种清理。`--help` 不启动任何验证，仍可查看。真实 `scripts/verify.sh full` 退出 0（含 fast 的三个步骤）。
- 本机私有证据：`.plan/ego-t13-t18-20261122/` 下的 `t15-disk-guard-final.log`、`t15-disk-report-after-fix.txt`、`t15-disk-report-long-budget.txt`、`t15-preflight-final.log`、`verify-full-final.log`。

## 6. 交付

- 原变更文件：`AGENTS.md`、`project-conventions.md`、`scripts/disk-guard.sh`、`scripts/verify.sh`、`TASK.md`、本文件。本次修复涉及 `scripts/disk-guard.sh`、`tests/disk_guard.test.sh` 和验收记录；低空间预检与 T-14 的 `verify.sh` 联合复验。
- 已依据第 5 节独立证据关闭规则措辞、报告超时与清理边界问题。验收通过不构成对任何真实删除的授权。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |

- 回滚：还原上述文件即可；脚本无持久状态。

## 7. 详细证据链

- 2026-11-18 22:56：`bash -n scripts/disk-guard.sh scripts/verify.sh` 通过。
- 2026-11-18 22:56：`bash scripts/disk-guard.sh` 输出主机 10.1 GiB 可用、Docker 镜像 20 个/7.506GB、`mcpdb-*` 卷 4 个；`--check` 返回 0。
- 2026-11-18 22:57：`--prune` 只检查停止的 `mcpdb-*` 容器，未发现对象；`--prune-cache` 在确认提示处中止，未执行删除。
- 2026-11-18 23:05：`scripts/verify.sh fast` 在 `rust:1.89-slim-bookworm` 容器内完成 fmt/clippy/test，`OK fast (423s)`，退出 0。
- 用户要求（m04363）：复盘当天耗时、Bug、返工、回滚和重复修改，分析共同根因，并沉淀规则/自动化/Skill；用户于 m04408 确认执行范围。