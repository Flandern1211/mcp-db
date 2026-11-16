# T-03 迁移 legacy v1 配置与状态库

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：跨 TOML/SQLite 数据迁移与破坏性兼容切换 |
| 状态 | 未开始 |
| 依赖 | T-02 |
| AC | AC2-23 |
| 基线 | Rust 基线治理提交（待回写） |
| 任务分支 | `feat/zcode/t-03-legacy-migration` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | — |

## 1. 范围

- 迁移源为 legacy Bun/TypeScript v1 的 TOML 与状态库，目标是 Rust AC2 状态模型。
- 实现停服维护锁、快照、staging 导入、持久 journal、明确提交点和 recovery。
- 按 SPEC 固定矩阵迁移旧 source/agent modes，不生成 provision 能力；旧 source 默认 disabled。
- 原子替换活动 TOML，使旧 Bun/TypeScript binary 不能从活动配置恢复权限，并验证 `execute_sql` 退役。
- 不实现新运行时 grants 路由（T-04）或 SQL 分类（T-05）。

## 2. 验收例子

1. 旧三种 mode 按固定矩阵生成 grants/策略，且从不生成 `create_database`。
2. 任一 cutover 故障点只能回滚提交点前状态、完成已提交切换或进入 recovery，服务不能以混合授权启动。
3. 成功后活动 TOML 无旧 sources/agents/DSN/token 配置，旧 Bun/TypeScript binary 无法取得 source 权限。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 5.3、AC2-23；开工前固定 journal 状态机、提交点和旧敏感配置隔离路径。
- 步骤：迁移 spike → legacy fixture/harness → staging 校验 → cutover/recovery → 崩溃注入矩阵。
- 拆分结论：迁移拥有独立失败/回滚边界，不能并入运行时授权。
- 并行判定：T-02 合入后可与 T-04 的非重叠部分并行；修改配置/状态契约时串行。
- 风险：跨文件非原子切换；用 journal、fsync/rename 和禁启动 recovery 缓解。
- 第一性原理检查：授权 source of truth 任一时刻只能有一个。
- 对抗性审查：覆盖旧 binary、崩溃与遗留敏感备份路径。
- 奥卡姆检查：不支持自动降级或双写期。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| E2E/故障注入 | AC2-23 | 旧夹具迁移与各 cutover 点崩溃矩阵 | 临时目录 + legacy v1 夹具 | 未开始 | 全部 |

外部写调用数：0。证据位置：本任务文件（仓库可见）。

## 5. 交付

- 提交与 `TASK.md`：未开始。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未测试 | AC2-23 | 任务未开始，迁移夹具和 journal 尚未建立 | 不能证明旧配置安全切换 | 建立夹具与崩溃注入矩阵并执行 | 是 |
