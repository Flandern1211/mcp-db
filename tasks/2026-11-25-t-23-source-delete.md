# T-23 数据源删除（物理删除控制面记录）

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整（用户可见行为、数据模型与 AC2-21 基线变更、跨模块：store/admin/CLI/前端） |
| 状态 | 🟡 待验收 |
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
- `DELETE /admin/api/sources/{id}`（写守卫：会话 + CSRF + Origin），事务提交后清理无其他 profile 引用的托管凭证，失败降级为 `warnings`。
- 管理页：列表与详情页新增「删除」入口与二次确认弹窗（必须输入数据源 ID 才能确认）。
- CLI：`mcp-db sources delete <id>`，只清控制面记录并逐条提示保留的凭证引用。
- 基线：`SPEC.md` §3.6 与 AC2-21、`docs/PRD.md` §3.5/§4.1/§5/§7、`CHANGELOG.md`、`README.md` CLI 列表。

不做什么：

- 不执行 `DROP DATABASE` 或任何外部写；不删除 `audit_events`、`approval_requests` 历史行。
- 不新增 `deleted` 状态，不改 `status` CHECK 约束。
- 不改变归档/恢复语义与 AC2-19 配额口径。

## 2. 验收例子

1. 已归档数据源在列表和详情页同时显示「恢复」和「删除」；启用中的数据源同时显示「归档」和「删除」。
2. 删除弹窗未输入匹配 ID 时确认按钮禁用；输入正确 ID 后确认，`DELETE /admin/api/sources/{id}` 返回 200，列表与概览不再包含该数据源，`GET /admin/api/sources/{id}` 返回 404。
3. 删除后状态库不再有该 source 的 profiles、routes、grants；经管理页删除时无其他 profile 引用的托管凭证被移除；`SOURCE_DELETED` 审计可查询。
4. 存在待批/已批准审批、子数据源或未结算建库预留时返回 409 与可读原因，且该 source 及关联记录全部保持不变。
5. 删除只清控制面记录，不产生任何数据库连接或外部写；同名 ID 可以重新创建。
6. CLI `mcp-db sources delete <id>` 与 Web 使用同一 store 逻辑与同一拒绝条件。

## 3. 计划与风险

- 设计依据：`SPEC.md` §3.6、AC2-21；`docs/PRD.md` §3.5；现有 `archive_source`/`restore_source` 的事务与审计写法。
- 外键清单（删除顺序依据）：`connection_profiles.source_id`、`source_tool_routes.source_id`、`tool_grants.source_id`、`quota_reservations.parent_source_id`、`sources.parent_source_id`。`approval_requests.source_id` 与 `audit_events.source_id` 无外键，历史保留。
- 对抗性检查：删除与审批并发（同事务内校验）；父 source 的子 source 已删但预留仍指向它；删除后 `provision_usage` 不得把已删库计为活动；凭证清理失败不得回滚已完成的删除，也不得静默成功。
- 奥卡姆检查：不引入软删除状态、不新建审计表、不改 MCP 工具契约、不为删除加独立权限模型。
- 已知取舍：凭证清理在数据库事务提交后执行；若清理失败，数据源已删除，响应携带 warning，由管理员检查凭证库。
- CLI 不清理凭证：运行中的服务以 `EncryptedFileResolver` 持有内存副本并整文件重写，CLI 作为第二个写者会被覆盖或覆盖服务刚写入的凭证。CLI 删除后逐条 stderr 提示保留的引用，凭证由管理页删除时清理。已由实现过程发现并回退（见第 4 节）。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 |
| --- | --- | --- | --- | --- |
| 模块集成（store） | 2、3、4、5 | `bash scripts/verify.sh fast`（内含 `cargo test --locked`） | 本地 Docker（rust:1.89-slim-bookworm） | `tests/source_delete.rs` 10 例全过：级联与 ID 复用、SOURCE_DELETED 审计（decision=allowed）、三种拒绝条件且记录不变、已结算预留随删、quarantined 双列拒绝、归档可删、NotFound、token 身份不受影响 |
| HTTP 集成 | 2、4 | 同上 `cargo test --locked` | 同上 | `tests/admin_http.rs` 新增 2 例：401/403 守卫矩阵；pending 审批 409（中文原因）→ deny 后 200，详情 404、列表空、overview total=0 |
| 凭证库 | 3 | 同上 `cargo test --locked` | 同上 | `tests/credentials.rs` 新增 2 例：remove 后其余引用可解析且重启持久；未知引用幂等 Ok |
| 前端（Node Mock） | 1、2 | `node --test tests/admin_source_delete.test.cjs tests/admin_grants_picker.test.cjs tests/admin_token_copy.test.cjs` | 本机 Node v24 | 20/20 通过：active 有归档+删除、archived 有恢复+删除；弹窗 ID 门禁（错 ID disabled、正确 ID enabled、trim）；成功路径恰好一次 DELETE + 列表重载 + 弹窗关闭；失败路径弹窗保持并显示服务端中文原因 |
| 静态检查 | — | `node --check src/transport/admin_assets/app.js`、`git diff --check` | 本机 | 均通过 |
| 推送门禁 | — | `bash scripts/preflight.sh` | 本机 Docker | OK（0 warning） |
| 真实入口（干净容器） | 2、3、5、6 | 新建临时目录 + 新随机主密钥 → `docker build -t mcp-db:t23 .` → `docker run`（全新 `/data`，不复用旧状态） | 本地 Docker，镜像 `mcp-db:t23` | 容器 healthy；CLI `sources add` + `sources delete` 退出码 0 并输出 `deleted … (physical database untouched)`；Web 路径：登录 → 建 source → 建带 DSN 的 profile → `DELETE /admin/api/sources/web1` 返回 200 `{"deleted":true,…,"warnings":[]}`，随后 `GET` 该 source 返回 404，`credentials.enc` 从 300+ 字节回到 36 字节（空库）；审计出现 `deleted`/`SOURCE_DELETED`/`allowed` 与 `admin_source_deleted`；删除不存在的 id 返回 404 中文提示 |
| 反复执行 | 6 | 同一容器内 `sources add` + `sources delete` 循环 20 次 | 同上 | 20/20 无 warning，无失败 |

提交：`9804455`（实现）、`b53e2bb`（证据回填）、`754ace2`（TASK.md 记账）、`27d9474`（归档按钮改文字）。

审查修正（主会话复核实现席 diff 后）：

1. `restore` handler 响应体被误改为裸对象，已恢复 `{"source": …}` 原契约。
2. `delete_source` handler 解构与 JSON 重组改为具名变量，行为不变。
3. 列表归档按钮原本是垃圾桶图标，与新增的「删除」文字按钮语义冲突，改为文字「归档」。
4. **CLI 凭证清理整段移除**：实现席让 CLI 直接打开凭证库删除引用，但运行中的服务是凭证库唯一写者并持有内存副本、整文件重写，CLI 作为第二写者会被覆盖或覆盖服务刚写入的凭证。改为 CLI 只清控制面记录并逐条提示保留的引用，凭证由管理页删除时清理；SPEC/PRD/README/CHANGELOG 同步收窄措辞。

## 5. 缺口表

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未验收 | 删除入口的人工验收（浏览器真实点击删除弹窗） | 主会话已在干净容器内用真实 HTTP 走通登录→建源→建 profile→删除→核对 404/列表/审计/凭证文件，但这是实现方证据，按 AGENTS.md §9.12 不能替代第二方验收；浏览器 UI 渲染未由第二方查看 | 影响 AC2-21 删除部分的人工签字；不影响合并与后续任务 | 用户或独立会话打开管理页，对活动态与归档态各删一个测试数据源，确认按钮文案、弹窗 ID 门禁、删除后列表与概览消失 | 否 |
| 🔴 未测试 | 凭证清理失败 → 200 + warnings 的端到端路径 | fixture 固定传 `None` sink，需要新的失败注入点才能自动化 | 仅影响该 warning 分支的自动化证据；逻辑已审查且代码路径简单 | 后续给 AdminState 加可注入失败 sink 的测试构造再补一例；或人工在测试环境把凭证库设为只读 | 否 |

无阻塞后续的缺口。
