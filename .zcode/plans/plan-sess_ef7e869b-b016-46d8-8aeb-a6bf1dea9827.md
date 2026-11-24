## Context

上一版用 `ro-guard.sh` 补"只读"，但审计发现它挡不住有意绕过：`git config` 可直接写配置、`diff --output` 可写任意文件、`branch -f` / `remote rename` 漏网、`GIT_DIR` 等环境变量可把审计指向另一个仓库。它给的是虚假安全感。

同时确认了 ZCode 的能力边界：hooks 有 `PreToolUse`、可用退出码 2 或 JSON `deny` 阻断，但 **matcher 只能按工具名匹配，无法区分是哪个子 agent**；agent frontmatter 也没有 `disallowedTools` / `permissionMode` / `sandbox`。所以"按 agent 强制只读"在 harness 层面做不到。

**结论：只能靠结构，不靠规则。** 审查席去掉 Bash（没有 Bash 就没有写能力）；验证席必须跑命令，改用一次性 worktree，写也只写到副本里。

## 设计决定

1. **审查席 = Read + Grep + Glob，没有 Bash。** 硬保证：无进程、无重定向、无写文件。代价是它不能自己跑 `git diff`，由主会话预先产出审查包（diff 文件 + 变更文件清单 + 验收例子）。
2. **验证席保留 Bash，但只准在一次性 worktree 里跑。** 主会话派活前记录主仓库 `git status --porcelain` 基线，验证席交付后对比；worktree 用完即删。写入发生在副本，主仓库按构造不受影响。
3. **删除 `tools/ro-guard.sh`。** 它既不再被需要，又无法修到可靠；保留只会让人以为已经隔离。
4. **独立 PR Review = 同一个审查席的直接入口，不是第二个 Agent。** 只读席位无副作用，可以直接派；输入退化为最小包（diff + 变更文件 + PR 描述或验收例子）。写代码的席位仍必须走主会话，否则没有任务记录可追溯。
5. **主会话仍是唯一指挥。** 直接派审查只用于"看"，不用于"改"；闸门标记、缺口裁决、提交、记录仍在主会话。

## 改动

### 只读与验证

- `adapters/zcode/reviewer.md`：`tools: [Read, Grep, Glob]`，`injectAgentsMd: false`（避免业务项目"改完提交"的指令覆盖只读契约）；正文只指向 SEAT 和审查包。
- `adapters/zcode/verifier.md`：保留 `[Read, Grep, Glob, Bash]`，`injectAgentsMd: false`；正文加"只在一次性 worktree 内执行，交付时报告 worktree 路径"。
- `seats/reviewer/SEAT.md`：重写"只读怎么保证"——无 Bash、只读三类工具、输入是审查包；删除守卫相关条款。
- `seats/verifier/SEAT.md`：重写"只读怎么保证"——一次性 worktree、主仓库基线对比、worktree 用完即删。
- `tools/make-review-bundle.sh`（新增）：主会话专用，接收 `仓库 基线 目标 输出目录`，产出 `diff.patch`、`files.txt`、`stat.txt`。只读仓库，只写输出目录。
- 删除 `tools/ro-guard.sh` 与旧 `tools/README.md`，新 `tools/README.md` 说明审查包生成与 worktree 流程。

### 独立审查入口

- `adapters/zcode/README.md`：加"直接审查"用法——主会话生成 bundle 后直接派 `team-reviewer`，不走实现席；修正安装命令，只复制三个 agent 文件，不再 `*.md` 误把 README 装进去。
- `README.md`：加"两种入口"说明（流水线 / 直接审查），补全文件地图与 v0 描述。

### 修掉审计发现的硬矛盾

- `handoff.md`：补齐下游真正需要的字段（仓库路径、基线提交、diff 文件路径、待跑命令、环境前提、预判闸门）；明确区分**上行包**（派活）和**回执**（审查/验证结论），不再声称"每一跳同一格式"；"干净"定义为"与派活前基线一致"，不是"空"。
- `gates.md`：闸门编号改 `G1–G7`，与审查 rubric 的 `R1–R7` 分开，修掉"第 6 条 / 第 7 条"错位。
- `seats/reviewer/SEAT.md` 的 rubric 编号同步改 `R1–R7`。
- `seats/*/cases/*.md`：输入改成"在一次性 worktree 中还原缺陷版本"，不再对主仓库执行 `git -C`。
- `context.md`：把"只有三样输入"改成准确表述（简报、可读文件、命令输出；实现席另有注入的项目 AGENTS.md，审查/验证席已关闭注入）。
- `capabilities.md` 与模板字段对齐；`README.md` 去掉"没有版本号"与"v0"的矛盾。

## 验证

1. 用 `make-review-bundle.sh` 对 MCPDB 生成一份审查包，确认只写输出目录、不碰仓库。
2. 派 `team-reviewer` 跑 C01：确认它拿不到 Bash（尝试执行命令会被工具层拒绝），且能只凭 bundle 给出 blocking 与"人该看哪几处"。
3. 派 `team-verifier` 在一个一次性 worktree 里跑 V02 的准备步骤；结束后确认主仓库 `git status --porcelain` 与派活前基线逐字一致。
4. 检查安装：只复制 `implementer.md` `reviewer.md` `verifier.md` 三个文件，`~/.zcode/agents/` 不出现 `README.md`。
5. 结果写入对应 case 的"结果"字段与 `log.md`。

## 不做

- 不接入 OCR（按你的安排后续再加）。
- 不给验证席挂 MCP；V02 暂用 Bash + curl/CLI 执行，接入 MCP 单独评估。
- 不 `git init`，不装全局适配器（这两项等你单独确认）。