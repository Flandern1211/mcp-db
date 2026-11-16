# 任务表

任务状态、档位和证据位置的唯一入口。轻档只在本表登记；完整档在 `tasks/` 下建立任务文件，并在此链接。

当前产品基线见 [docs/PRD.md](docs/PRD.md)，系统基线见 [SPEC.md](SPEC.md)。Rust 基线治理提交为 `57041e4`，任务基线回写为 `c78ee6d`。T-02～T-11 按 Rust AC2 基线重规划；T-01 保留为 legacy Bun/TypeScript v1 历史任务。

## 任务表

| 编号 | 任务 | 档位 | 状态 | 依赖 | AC | 提交 | 未关闭项 | 证据/任务文件 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| T-01 | 实现 mcp-db v1 服务端 | 完整 | 🟡 待验收 | 无 | AC-01～AC-11 | `eb4a950`、`ec56fa4` | 2 非阻塞 | [tasks/2026-11-11-t-01-mcp-db-v1.md](tasks/2026-11-11-t-01-mcp-db-v1.md) |
| T-02 | 建立 Rust 工程与 AC2 状态/身份基础 | 完整 | 🟢 已完成 | 无 | AC2-01、AC2-09、AC2-11、AC2-22（基础） | `0f4a3c6`、`dc528be` | 无 | [tasks/2026-11-13-t-02-state-identity.md](tasks/2026-11-13-t-02-state-identity.md) |
| T-03 | 迁移旧配置与状态库 | 完整 | 未开始 | T-02 | AC2-23 | — | — | [tasks/2026-11-13-t-03-legacy-migration.md](tasks/2026-11-13-t-03-legacy-migration.md) |
| T-04 | 实现精确 grants、profile 路由与发现 | 完整 | 🟢 已完成 | T-02、T-12 | AC2-12～AC2-14 | `7c8374c`、`2c7fddf`、`c995885`、`eb9b87c`、`57bae34` | 无 | [tasks/2026-11-13-t-04-grants-routing-discovery.md](tasks/2026-11-13-t-04-grants-routing-discovery.md) |
| T-05 | 实现 PostgreSQL SQL 策略与分类 tools（含 explain） | 完整 | 未开始 | T-04 | AC2-03、AC2-06、AC2-07、AC2-15 | — | — | [tasks/2026-11-13-t-05-sql-tools.md](tasks/2026-11-13-t-05-sql-tools.md) |
| T-06 | 实现审批状态机与审计绑定 | 完整 | 未开始 | T-02、T-04、T-05 | AC2-04、AC2-05、AC2-09、AC2-16、AC2-22 | — | — | [tasks/2026-11-13-t-06-approval-audit.md](tasks/2026-11-13-t-06-approval-audit.md) |
| T-07 | 实现 PostgreSQL profile 限制与只读边界 | 完整 | 未开始 | T-04、T-05 | AC2-02、AC2-08、AC2-11 | — | — | [tasks/2026-11-13-t-07-profile-limits.md](tasks/2026-11-13-t-07-profile-limits.md) |
| T-08 | 实现 PostgreSQL 建库、配额与 source 生命周期 | 完整 | 未开始 | T-04～T-07 | AC2-17～AC2-21 | — | — | [tasks/2026-11-13-t-08-provisioning-lifecycle.md](tasks/2026-11-13-t-08-provisioning-lifecycle.md) |
| T-09 | 实现首版本机 CLI 与 Web 管理面 | 完整 | 未开始 | T-02、T-06、T-08 | AC2-10（首版本机）、AC2-21、AC2-22 | — | — | [tasks/2026-11-13-t-09-local-admin.md](tasks/2026-11-13-t-09-local-admin.md) |
| T-10 | 完成首版 AC2 跨边界验收与发布收口 | 完整 | 未开始 | T-03～T-09 | AC2-01～AC2-23（首版 PostgreSQL 范围） | — | — | [tasks/2026-11-13-t-10-ac2-acceptance.md](tasks/2026-11-13-t-10-ac2-acceptance.md) |
| T-11 | 扩展 MySQL/MariaDB、SQLite 与 SQL Server | 完整 | 未开始 | T-10 | AC2-02、AC2-08、AC2-11 增量驱动范围 | — | — | [tasks/2026-11-16-t-11-additional-drivers.md](tasks/2026-11-16-t-11-additional-drivers.md) |
| T-12 | 实现凭证解析与 PostgreSQL 元数据 provider | 完整 | 🟢 已完成 | T-02 | AC2-01、AC2-09、AC2-11、AC2-12（凭证与元数据基础） | `c790f64`、`49883ea` | 无 | [tasks/2026-11-16-t-12-credentials-postgres-metadata.md](tasks/2026-11-16-t-12-credentials-postgres-metadata.md) |

## 状态定义

| 状态 | 判定 |
| --- | --- |
| 未开始 | 尚未开展实现或验证。 |
| 进行中 | 已开始实现，尚未完成当前任务要求的验证。 |
| 🟡 待验收 | 实现完成；有未关闭项，但全部不阻塞后续工作。 |
| 🔴 受阻 | 至少一项未关闭项阻塞合并、发布或依赖任务；或外部条件缺失无法继续。 |
| 🟢 已完成 | 缺口表为 `🔴 无`，或所有未关闭项已关闭/用户明确接受风险。 |

## 维护规则

1. 开工时判断档位并写一行理由；轻档为默认，命中完整档条件才升级。
2. 轻档证据写提交正文：AC、命令或步骤、环境、结果、`Unverified` 行（项目、阻塞/非阻塞、动作、关闭条件）；本表记状态、未关闭项计数、提交和证据位置。
3. 完整档在 `tasks/` 下建立 `YYYY-MM-DD-t-xx-name.md`，写范围、验收例子、计划与风险、测试证据、6 列缺口表、已接受风险；本表链接。
4. `未关闭项` 列必填：无写「无」，有写 `1 非阻塞` 或 `2 阻塞、1 非阻塞`；状态用 `🟢 已完成 / 🟡 待验收 / 🔴 受阻`，未开始/进行中保持文字。
5. 未关闭项阻止完成：只有缺口表为 `🔴 无` 或用户明确接受风险时才能标 `🟢 已完成`；未关闭项超过 3 个时不交付。
6. `TASK.md` 只在集成分支（默认 `develop`）由主 agent 维护；允许仅含本表的记账提交。worktree 和任务分支使用本地 `.plan/T-xx.md`，合并时由主 agent 更新本表。
7. 不设独立的 docs 提交；基线变化才更新 `SPEC.md` 和 `CHANGELOG.md`。
8. 提交存在、代码已写或文档已更新都不等于完成；验收追问和缺口关闭结果要回写本表。