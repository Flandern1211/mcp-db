# T-04 实现精确 grants、PostgreSQL profile 路由与发现

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：对外 MCP 契约与跨模块授权改造 |
| 状态 | 未开始 |
| 依赖 | T-02 |
| AC | AC2-03、AC2-12～AC2-14 |
| 基线 | Rust 基线治理提交（待回写） |
| 任务分支 | `feat/zcode/t-04-grants-routing-discovery` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | — |

## 1. 范围

- 每次调用复核 `(token, source, tool)` grant，并解析 PostgreSQL 的明确 route/profile；缺失或停用不回退。
- 工具列表为有效 grants 的并集；`list_sources` 只返回有效且未归档 source。
- 实现 `list_sources`、`list_tables`、`describe_table`、`explain_query` 的发现与调用门禁。
- 生产普通写保存和调用均拒绝；定义新 source 的会话刷新机制。
- 不实现 SQL AST 策略（T-05）、审批消费（T-06）或建库（T-08）。

## 2. 验收例子

1. A source 可见的 tool 对 B source 直接构造调用仍拒绝，目标驱动调用数为 0。
2. PostgreSQL tool route 缺失、profile 停用或用途不符均拒绝且不升级身份。
3. grants 或新 source 变化后，工具并集和 source 视图按确定的会话刷新语义更新。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 3.1～3.3、5.1；开工前确定 `tools/list_changed` 或重建会话策略。
- 步骤：授权服务 → PostgreSQL profile pool key → 动态 schemas/发现 → MCP 会话复核 → 测试。
- 拆分结论：SQL 解析和审批有独立契约，分至 T-05/T-06。
- 并行判定：与 T-03 仅在不触碰共享配置/状态契约时并行。
- 风险：静态 source/tool schema 缓存越权；移除授权决定中的静态枚举依赖。
- 第一性原理检查：可见性不是授权，调用时必须复核。
- 对抗性审查：覆盖伪造 source、旧会话与状态变更。
- 奥卡姆检查：不做授权缓存。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 模块集成/E2E | AC2-03、AC2-12～14 | MCP 发现、伪造调用、状态变化矩阵 | Mock driver + 临时 SQLite | 未开始 | 全部 |

外部写调用数：0。证据位置：本任务文件（仓库可见）。

## 5. 交付

- 提交与 `TASK.md`：未开始。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未测试 | AC2-03、12～14 | 任务未开始 | 授权与发现无实现证据 | 按范围实现并执行发现/伪造调用矩阵 | 是 |
