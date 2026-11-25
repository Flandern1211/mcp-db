# T-23 数据源删除（物理删除控制面记录）

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整（用户可见行为、数据模型与 AC2-21 基线变更、跨模块：store/admin/CLI/前端） |
| 状态 | 进行中 |
| 依赖 | T-08（source 生命周期）、T-09/T-13（管理面） |
| AC | AC2-21（修订：归档之外新增物理删除） |
| 任务分支 | `feat/zcode/t-23-source-delete` |
| 合并目标 | `develop` → `main` |
| Base | `da03f60`（develop） |
| worktree | 无（主工作区顺序执行） |

## 0. 需求与决策

问题：管理页只有「归档」，归档后的数据源永远显示「恢复」，无法真正删除。

用户 2026-11-25 确认的三项决策：

1. 语义为**物理删除控制面记录**：删除 `sources` 行及其 profiles/routes/grants 和服务端托管凭证；审计保留（`audit_events`、`approval_requests` 对 source 无外键）；物理数据库永不删除。
2. 归档保留，删除独立且随时可用（活动态与归档态都可删）。
3. 存在未完成事项时**拒绝并提示**，不做强制级联。

未完成事项（拒绝条件）：

- 该 source 存在 `pending`/`approved` 审批；
- 该 source 仍有子 source；
- 该 source 相关（`parent_source_id = id` 或 `database_name = id`）存在 `reserved`/`quarantined` 预留。

已结算（`committed`/`released`）预留不算未完成事项：随删除清理对应行，使 `provision_usage` 不再把已删数据源计为活动库（`usage_from_conn` 对 `s.id IS NULL` 计为 active）。

## 1. 范围

做什么：

- `Store::delete_source`：单事务校验 + 级联删除 routes → profiles → grants → 相关预留 → source，写 `SOURCE_DELETED` 审计。
- `CredentialSink::remove`：删除加密凭证库中的引用（幂等，缺失不算错误）；只清理服务端托管引用 `keyring:managed/<id>/…` 与 `keyring:provision/managed/<id>/…`，不动 CLI 手工写入的任意引用（可能被多个 profile 复用）。
- `DELETE /admin/api/sources/{id}`（写守卫：会话 + CSRF + Origin）。
- 管理页：列表与详情页新增「删除」入口与二次确认弹窗（必须输入数据源 ID 才能确认）。
- CLI：`mcp-db sources delete <id>`。
- 基线：`SPEC.md` §3.6 与 AC2-21、`docs/PRD.md` §3.5/§4.1/§5/§7、`CHANGELOG.md`。

不做什么：

- 不执行 `DROP DATABASE` 或任何外部写；不删除 `audit_events`、`approval_requests` 历史行。
- 不新增 `deleted` 状态，不改 `status` CHECK 约束。
- 不改变归档/恢复语义与 AC2-19 配额口径。

## 2. 验收例子

1. 已归档数据源在列表和详情页同时显示「恢复」和「删除」；启用中的数据源同时显示「归档」和「删除」。
2. 删除弹窗未输入匹配 ID 时确认按钮禁用；输入正确 ID 后确认，`DELETE /admin/api/sources/{id}` 返回 200，列表与概览不再包含该数据源，`GET /admin/api/sources/{id}` 返回 404。
3. 删除后状态库不再有该 source 的 profiles、routes、grants；服务端托管凭证被移除；`SOURCE_DELETED` 审计可查询。
4. 存在待批/已批准审批、子数据源或未结算建库预留时返回 409 与可读原因，且该 source 及关联记录全部保持不变。
5. 删除只清控制面记录，不产生任何数据库连接或外部写；同名 ID 可以重新创建。
6. CLI `mcp-db sources delete <id>` 与 Web 使用同一 store 逻辑与同一拒绝条件。

## 3. 计划与风险

- 设计依据：`SPEC.md` §3.6、AC2-21；`docs/PRD.md` §3.5；现有 `archive_source`/`restore_source` 的事务与审计写法。
- 外键清单（删除顺序依据）：`connection_profiles.source_id`、`source_tool_routes.source_id`、`tool_grants.source_id`、`quota_reservations.parent_source_id`、`sources.parent_source_id`。`approval_requests.source_id` 与 `audit_events.source_id` 无外键，历史保留。
- 对抗性检查：删除与审批并发（同事务内校验）；父 source 的子 source 已删但预留仍指向它；删除后 `provision_usage` 不得把已删库计为活动；凭证清理失败不得回滚已完成的删除，也不得静默成功。
- 奥卡姆检查：不引入软删除状态、不新建审计表、不改 MCP 工具契约、不为删除加独立权限模型。
- 已知取舍：凭证清理在数据库事务提交后执行；若清理失败，数据源已删除，响应携带 warning，由管理员检查凭证库。

## 4. 测试与证据

（实现完成后回填）

## 5. 缺口表

（交付时回填）
