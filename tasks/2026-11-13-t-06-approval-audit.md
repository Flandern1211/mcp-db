# T-06 实现审批状态机与审计绑定

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：高风险写入审批、一次性消费与持久状态 |
| 状态 | 🟡 待验收 |
| 依赖 | T-02、T-04、T-05 |
| AC | AC2-04、AC2-05、AC2-09、AC2-16、AC2-22 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-06-approval-audit` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | 待合入 |

## 1. 范围

- 绑定 token/source/tool/完整参数/PostgreSQL profile/route 版本的不可变摘要与一次性消费。
- 普通 elicitation 与 admin 队列分流；高危强制 admin；执行前重验全部状态。
- 审计身份、profile、决定、渠道、原因和适用耗时/行数；未知结果不重试。
- 不实现建库配额（T-08）或 Web 管理面（T-09）。

## 2. 验收例子

1. elicitation accept 恰好执行一次，decline/cancel 执行 0 次。
2. 高危 SQL 即使 direct 也进入 admin；批准后撤销 token 或变更 route 时消费失败。
3. 并发/重复消费只有一次成功；重启不执行旧 pending/approved，未知结果保留。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 2、3.3、3.4、5.2。
- 步骤：摘要规范 → 状态机/schema → 渠道编排 → 执行前复核 → 审计/重启测试。
- 拆分结论：建库预留有额外边界，归 T-08；管理交互归 T-09。
- 并行判定：依赖共享执行契约，串行。
- 风险：批准与执行竞态；使用事务条件更新和执行前最终复核。
- 第一性原理检查：批准只授权精确请求的一次尝试。
- 对抗性审查：覆盖篡改、撤销、归档、版本变化和重启。
- 奥卡姆检查：不实现通用工作流引擎。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 模块集成 | AC2-04、05、09、16、22 | `cargo test --test approval_store` | 临时 SQLite | 6 passed：批准/拒绝/过期/一次性消费；摘要不符；token 撤销；route 版本变化失效；重启 pending/approved 失效、in-flight unknown；unknown 结果不可覆盖；审计事件 | 无 |
| MCP/E2E | AC2-04、05、16 | `cargo test --test http_mcp` | Mock executor + 临时 SQLite | 7 passed：elicitation accept 执行一次；decline/cancel 不执行；高危与 approval grant 进入 admin；admin 批准后 retry 只消费一次；重复 retry 被拒 | 真实 MCP 客户端 elicitation 未验证 |
| 全量回归 | 工程基线 | `cargo fmt --check`；clippy driver；`cargo test` | Docker Desktop Linux 容器 | 47 passed, 6 ignored；lint 通过 | 无 |

外部写调用数：0（Mock）。证据位置：本任务文件和提交正文（仓库可见）。

实现说明：

- 新增 `approval_requests` 与 `audit_events`，schema v2；legacy `approvals`/`audit` 保持只读历史。
- 摘要绑定 token/agent/source/environment/tool/SQL/参数/profile 版本/route 版本/grant 策略/route 最低策略/高危标记。
- admin pending 通过 `approvalId` 返回；批准后 agent 用同一请求 + `approval_id` 重试，claim 原子消费一次。
- elicitation 由 `ElicitationGate` 抽象，生产实现使用 rmcp `Peer`；测试用 fake gate 覆盖 accept/decline/cancel。
- 高危 SQL 和 `create_database` 永不接受 elicitation，只进入 admin。
- 执行错误在 claim 后记录 `unknown`，不自动重试。

## 5. 交付

- 提交与 `TASK.md`：`47a531a`；待合入 `develop` 并回写状态。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未测试 | 真实 MCP 客户端 elicitation | 生产 `PeerElicitation` 已实现，但当前测试使用 fake gate；没有真实客户端 accept/decline/cancel 会话记录 | 真实协议交互未验证；不影响状态机和 admin 队列 | 在支持 elicitation 的 MCP 客户端执行 accept/decline/cancel 并记录会话；关闭条件：三条真实交互证据 | 否 |
