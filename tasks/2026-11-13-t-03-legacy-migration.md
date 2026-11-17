# T-03 迁移 legacy v1 配置与状态库

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：跨 TOML/SQLite 数据迁移与破坏性兼容切换 |
| 状态 | 🟢 已完成 |
| 依赖 | T-02 |
| AC | AC2-23 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-03-legacy-migration` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | `88c37f3` |

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
| 单元/模块集成 | AC2-23 | `cargo test --locked --test legacy_migration` | 临时目录 + legacy TOML/SQLite 夹具 | 10 passed：固定 grants 矩阵、无 `create_database`、source archived、三个 profiles、凭证可解析、活动 TOML 无 DSN/sources/agents、备份 0600、legacy approvals/audit 保留 | 无 |
| 故障注入 | AC2-23 | 同一测试内的 `FailPoint` 崩溃点 + recovery | 临时目录 | 提交点前失败 → 回滚且启动保护解除；状态库 rename 后失败 → recovery 前滚完成 TOML 并 `done`；未完成 journal 阻止启动 | 无 |
| 拒绝路径 | AC2-23 | 非 postgres driver、缺 token env、重复迁移、WAL 未 checkpoint、production 与写模式冲突 | 临时目录 | 均在写入目标前失败，目标状态库与 TOML 未改变 | 无 |
| 构建与静态检查 | 工程基线 | `cargo fmt --check`；`cargo test --locked`；clippy；`cargo build --release --locked` | Docker Linux | 114 passed, 11 ignored；clippy 严格通过；release 构建通过 | 无 |

外部写调用数：0；仅本地临时目录与加密凭证文件。
证据位置：本任务文件；夹具为仓库内测试代码，不含真实 DSN/token。
边界：未运行旧 Bun binary 的真实端到端；活动 TOML 的“无 source/agent 权限”由内容断言与 legacy schema 语义证明。

## 5. 交付

- 提交与 `TASK.md`：`23c427e`，合入 `88c37f3`；🟢 已完成，未关闭项 0。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
