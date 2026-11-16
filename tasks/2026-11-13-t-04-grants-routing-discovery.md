# T-04 实现精确 grants、PostgreSQL profile 路由与发现

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：对外 MCP 契约与跨模块授权改造 |
| 状态 | 🟢 已完成 |
| 依赖 | T-02、T-12 |
| AC | AC2-12～AC2-14 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-04-grants-routing-discovery` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | `50762a3` |

## 1. 范围

- 每次调用复核 `(token, source, tool)` grant，并解析 PostgreSQL 的明确 route/profile；缺失或停用不回退。
- 工具列表为有效 grants 的并集；`list_sources` 只返回有效且未归档 source。
- 实现 `list_sources`、`list_tables`、`describe_table` 的 MCP 工具与调用门禁；元数据通过 `MetadataProvider` seam 获取，真实 PostgreSQL provider/凭证解析由 T-12 接入。
- `explain_query` 的 SQL 策略属于 T-05，不在本任务完成范围内；AC2-03 的普通写入执行归 T-05/T-06。
- 生产普通写保存和调用均拒绝。
- 会话刷新语义：每次 `tools/list` 和调用都从状态库重算，不缓存；当前会话重新请求 `tools/list` 即可看到变化，不实现主动 `tools/list_changed` 通知。
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
| 单元/模块集成 | AC2-03、AC2-12～14 | `cargo test --test authorization` | Docker Linux + 临时 SQLite | 8 passed：工具并集、精确 grant、source 推断、route/profile、生产写、禁用 profile、归档 source、动态刷新 | 无 |
| HTTP/MCP 集成 | AC2-01、AC2-12、AC2-13 | `cargo test --test http_mcp` | Docker Linux + axum/rmcp + Mock metadata | 3 passed：无 token 401；initialize/tools/list；`list_sources` 动态发现；跨 token session 403；四个工具契约与授权矩阵；未授权 source 返回 `TOOL_NOT_GRANTED` 且 provider 调用数不增加 | 真实 provider 归 T-12；explain 策略归 T-05 |
| 构建/容器 | 工程基线 | `cargo fmt --check`；`cargo clippy --all-targets --all-features -- -D warnings`；`cargo test`；`cargo build --release --locked`；`docker build`；容器健康检查 | Docker Desktop Linux 容器（Rust 1.89） | 26 tests passed；镜像构建通过；`user=10001:10001`、`health=healthy`；`/mcp` 无认证 401 | 无 |

外部写调用数：0。证据位置：本任务文件和提交正文（仓库可见）。

当前已实现授权事实、MCP transport、认证、会话绑定、四个工具的 schema/调用门禁和 Mock metadata 验证；T-12 已接入真实凭证解析和 PostgreSQL provider。T-04 的授权与发现范围已完成；`explain_query` 策略和普通写入执行分别由 T-05/T-06 承接。

## 5. 交付

- 提交与 `TASK.md`：`7c8374c`、`2c7fddf`、`c995885`、`eb9b87c`、`57bae34`，集成分支提交 `50762a3`；状态 🟢 已完成，未关闭项「无」。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
