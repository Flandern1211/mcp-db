# T-02 建立 AC2 状态模型与身份基础

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：涉及持久数据模型、身份和跨模块安全边界 |
| 状态 | 进行中 |
| 依赖 | 无 |
| AC | AC2-01、AC2-09、AC2-11、AC2-22 的持久化基础 |
| 基线 | `60aeb32` |
| 任务分支 | `feat/zcode/t-02-state-identity` |
| 合并目标 | `develop` |
| worktree | 无 |
| 集成分支提交 | — |

## 1. 范围

做什么：

- 建立版本化状态 schema，以及 source、profile、route、token、grant 的基础持久模型。
- token 只保存哈希和识别信息，支持撤销与重启后解析。
- 在写入边界拒绝未知 tool/profile、用途错配、非法凭据引用、production 普通写 grant 和 provision 越界路由。
- 保留旧 approvals/audit 历史，不让旧记录产生新权限。

不做什么：

- 不切换 MCP 工具执行路径，不迁移旧 TOML，不实现 SQL parser、审批消费、建库或远程管理面。
- 不把新旧权限合并读取；正式 cutover 分别归 T-03、T-04。

## 2. 验收例子

1. 签发 token 后状态库只有哈希；原始 token 可解析身份，未知 token 和撤销 token 被区分拒绝。
2. source、多个同用途 profiles、routes 和 grants 重启后仍存在，且唯一约束不产生隐式权限。
3. production source 的 `mutate_sql`/`execute_ddl` grant、用途错配 route、普通 tool 指向 provision profile 均在保存时失败。
4. 从旧状态库启动升级后，旧 approvals/audit 仍可读，schema 有明确版本，且没有凭空生成 token/grant。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：`SPEC.md` 3.1、3.2、4、5.2 与 AC2-01/09/11/22。
- 步骤：版本化 Store schema → 领域错误与模型写入校验 → 持久 token 解析/撤销 → 临时 SQLite 测试 → 全量回归。
- 拆分结论：本任务只交付后续任务共享的状态与身份地基；运行时切换和旧配置 cutover 可独立验收，故拆至 T-03/T-04。
- 并行判定：串行；T-03/T-04 依赖本任务 schema 契约。
- 风险：误让旧配置与新状态同时授权；缓解为本任务不接入现有 MCP 运行时。
- 第一性原理检查：目标是建立默认拒绝且可持久复核的授权事实；不可变约束是不存明文 token/凭证且不扩大旧权限；结论是先落最小状态契约，不提前切流量。
- 对抗性审查：即使直接写入越权记录，也必须由数据库约束或 Store 校验拒绝；重启不能恢复撤销 token。
- 奥卡姆检查：不新增管理 API、凭据后端抽象、缓存或兼容读取层。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元/模块集成 | AC2-01、AC2-09、AC2-11、AC2-22（基础） | `bun test tests/store.test.ts tests/auth.test.ts` | 本地临时 SQLite | 待执行 | 无 |
| 回归 | 同上 | `bun run typecheck`；`bun test` | 本地 | 待执行 | 无 |

外部写调用数：0。

证据位置：本任务文件和本地提交正文（仓库可见）。

## 5. 交付

- 提交与 `TASK.md`：待完成。
- 已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
