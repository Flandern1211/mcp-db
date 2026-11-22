# T-17 按数据源管理令牌授权

- 状态：🟢 已完成；完整档（管理写入、权限、跨模块及管理 API）。依赖 T-13、T-16 已集成代码。未关闭项：0。
- 起点 `3d4e3d1`（`develop`）；合并目标 `develop`；分支 `feat/zcode/t-17-source-grants`。
- UI 基线：用户在 2026-11-20 提供两张 Nexus 截图、批准「只改令牌页和弹窗、保留 Nexus 样式」的方案。隔离启动 `MSYS_NO_PATHCONV=1 docker run -d --name mcpdb-t17-baseline --tmpfs /data:uid=10001,gid=10001,mode=0700 -p 127.0.0.1:29787:8787 -p 127.0.0.1:29788:8788 mcp-db:t13-nexus1`，用 CLI 准备两源三授权，IAB 实际登录回环管理页并打开访问令牌；镜像不宣称与源码起点完全一致。基线判定：旧列表 `qa-nonprod` 随工具出现两次，旧弹窗按单条 grant 展开；新版同源只出现在一张卡片和一个列表标签，截图中其余风格保持。基线截图：`.plan/evidence/baseline-token-list.png`、`.plan/evidence/baseline-grants-modal.png`（本地忽略目录）。

## 上下文包

- 权威：[`SPEC.md`](../SPEC.md) §1、§3.1、§3.3、§5；[`docs/PRD.md`](../docs/PRD.md) §3.5。
- 契约：同源 `/admin/api/*`，按令牌 ID 与源 ID 一次保存；现有单项 API 和 CLI 保留。
- 不变量：数据仍按 `(token, source, tool)` 精确授权；Agent 名称只作标签；生产普通写入、路由约束与重新审批不降低。
- 验证入口：`bash scripts/verify.sh fast`、`node --check src/transport/admin_assets/app.js`、隔离本机管理页及 MCP。
- 证据位置：本文件验证记录、`tests/admin_http.rs`、状态库测试与本地 `.plan/T-17.md`。

## 范围与设计

1. 列表只显示去重的授权数据源。同名 Agent 的令牌按短标识区分；不做独立 Agent 身份、名称唯一、自动换发或数据迁移。
2. 管理授权每源一张可展开卡片，按工具勾选及设置策略。新增先选源，清空会提示；已授权但不可用项仍可显式取消，不静默删除或重新新增。
3. 按源一次保存和分组编辑使用同一契约，作为一个交付单元；签发复制独立为 T-18。用 SQLite 写事务校验打开时快照、权限和路由，原子应用差异、记录审计并失效受影响的未消费审批。并发修改拒绝覆盖；网络结果未知先读回确认。不开跨源保存、不改 MCP 契约。
4. 第一性：数据源是管理单位，工具仍是精确的生效单位。对抗性：用同名、过期快照、停用路由、生产非法项和清空场景验证不串权、不部分写、不静默覆盖。奥卡姆：复用现有授权表、读取接口、样式和校验，不新建数据源绑定表或依赖。

## 验收例子

1. 同一令牌在 `qa-nonprod` 的两条授权和 `qa-prod` 的一条授权，列表只显示两个源标签，弹窗同源一张卡片，展开后看到各自工具和策略；同名令牌不混用。
2. 同一源勾选、取消并调整多个工具，一次保存发一个请求，刷新后完整生效；清空最后一项后列表和该令牌的 `list_sources` 不再列出该源，其他源/令牌不变。
3. 缺 CSRF、生产普通写入、停用/缺路由、重复工具或旧快照均不可造成部分写或越权；未获授权的实际工具调用被拒绝。保留未改、另一作用域和已消费审批；被删或改策略的工具的未消费审批失效。
4. 键盘可展开和选工具，窄屏控件可见；已授权不可用项明确标注且可取消，不可新增。

## 验证与缺口

- 改动：`src/transport/admin_assets/{app.js,app.css}`（验收 1、2、4）；`src/transport/admin.rs`、`src/state/{store.rs,model.rs}`、`src/{mcp.rs,main.rs,error.rs}`（验收 2、3）；四个回归测试文件见下表；`SPEC.md`、`CHANGELOG.md` 记录管理面契约。

**环境与真实入口（2026-11-20）**：在 Windows Docker 中，从联合补丁快照构建 Linux `debug` 二进制；新建独立容器与空 tmpfs，用 CLI 准备虚构源、路由和令牌。未触碰日常管理实例，未连真实数据库。

```bash
MSYS_NO_PATHCONV=1 docker run -d --name mcpdb-t17-t18-preview --user 10001:10001 \
  --tmpfs /data:uid=10001,gid=10001,mode=0700 -v mcpdb-cargo-target-3985852265:/build:ro \
  -e MCPDB_STATE_PATH=/data/state.db -e MCPDB_ADDR=0.0.0.0:8787 -e MCPDB_ADMIN_ADDR=0.0.0.0:8788 \
  -p 127.0.0.1:29797:8787 -p 127.0.0.1:29798:8788 debian:bookworm-slim /build/debug/mcp-db
```

| 验收 | 层级与实际结果 | 证据 |
| --- | --- | --- |
| 1、4 | 已实现。旧界面复现有截图；新版列表去重、分组弹窗、键盘和窄屏无独立浏览器验收。 | `src/transport/admin_assets/app.js`、`src/transport/admin_assets/app.css`；基线截图见上；未验收项见下。 |
| 2 | 本机真实 HTTP/MCP：`GET /healthz` 成功；管理登录、签发、同源一次保存两工具成功；过期快照返回 409；清空后既有 MCP 会话 `list_sources` 不再含已清空源，另一源原授权不变，精确工具调用返回 `TOOL_NOT_GRANTED`。该验证不包含从页面发出请求。 | 本地 `.plan/T-17.md` 链接的 `http-acceptance.cjs`（在联合快照下执行 `node http-acceptance.cjs`）。 |
| 3 | 模块/HTTP 回归：整组非法变更失败且无部分写，准确作用域与审批失效规则通过；等待中的用户确认不能在撤销或策略恢复后沿用。 | `tests/admin_http.rs`、`tests/approval_store.rs`、`tests/http_mcp.rs`、`tests/state.rs`。 |

- 联合快照执行 `bash scripts/verify.sh fast`：锁定 Rust 1.89 的 Docker 入口；fmt、clippy、139 项常规测试通过，11 项真实外部集成测试按默认门禁忽略。`node --check src/transport/admin_assets/app.js` 与两工作区 `git diff --check` 通过。日志位置见 `.plan/T-17.md`。静态安全复核独立于实现；不替代界面验收。
- 集成提交与状态见 [`TASK.md`](../TASK.md)。本地合并不代替界面验收。

### 2026-11-22 独立 Ego Lite 验收

- 验收人：非原实现者的会话。环境：Windows Ego Lite 0.6.3.17（`ego-browser` 2.2.1），从 `51cfc25` 同镜像启动的隔离真实 HTTP 服务、空 tmpfs、虚构令牌和源、不接外部数据库。启动命令和输出见本地 `.plan/ego-t13-t18-20261122/README.md` 与 `t17-*.json`。
- **已有授权通过**：同名令牌分开计，四条夹具授权只形成三张源卡片与三个列表标签；390px 下页面和弹窗无横向溢出，Enter / Tab / Space 可操作卡片和工具。一次保存同源取消工具、添加工具、修改策略，只发一个 `PUT`，HTTP 200，重读完全一致；不串源或同名令牌。停用连接配置后，已有不可用授权仍可取消，未授权工具不可新增；生产源普通写入及 DDL 禁用。
- **清空联动通过**：第一次操作只出现确认、零写入；确认后仅一个空集合 `PUT`（200），对应卡片及列表标签消失；同一 MCP 会话 `list_sources` 不再返回该源，同一工具调用返回 `TOOL_NOT_GRANTED`，其它源仍存在。原自动化中的审批失效等分支本轮没有重新执行；未触发外部写入。
- **新增源失败（已修复）**：服务端返回 4 个启用源，未授权令牌下拉框只有占位项，选择按钮禁用，0 个授权 `PUT`。根因依据：Ego Lite 读取 `<select>` 时发现子节点是 `[object HTMLOptionElement]` 文本而非选项 → `src/transport/admin_assets/app.js:1203–1206` 将 `choices.map(...)` 返回的数组整体传给原生 `replaceChildren`，原生按 DOM IDL 把非 Node 参数字符串化 → 应逐个传入 `<option>`。

### 2026-11-22 修复与复验

- 修复：`src/transport/admin_assets/app.js:1205` 改为 `...choices.map(...)`，其余 12 处 `replaceChildren` 调用未动；新增 `tests/admin_grants_picker.test.cjs`，夹具按 DOM IDL 把非 Node 参数转成文本节点、`options` 只返回 option 子节点、`select.value` 由 option 的 `selected` 推导，`node --test` 4/4 通过（含「设置选中值后按钮可用、点击生成卡片且无写请求」的正向用例）；把 `value` 访问器改成恒 `undefined` 后该用例失败，确认断言不恒真。既有 `tests/admin_token_copy.test.cjs` 12/12 无回归。
- 复验环境：`bash scripts/verify.sh full` 通过后构建的 `mcp-db:dev`（`883f5321d065`），同名隔离容器、空 tmpfs、回环端口 30787/30788，CLI 准备 `qa-a`（非生产，路由 list_tables/describe_table）、`qa-b`（生产，路由 list_tables）、`qa-c`（非生产，无路由）。验收人：本次独立会话（非实现者）。
- 结果：界面签发的零授权令牌打开「管理授权」后，下拉框为占位项 + 3 个启用源，无游离文本节点；选源零写入、焦点到卡片；qa-a 勾两个工具一次 `PUT`（`expected` 为空、`grants` 2 项）返回 200；生产 qa-b 的 `mutate_sql`/`execute_ddl` 置灰并提示「生产数据源禁止普通写入和结构变更」；390px 下 document/dialog 均无横向溢出；Enter 折叠、Tab 到首个复选框；清空首击仅「确认清空并保存」零写入，第二击一个空集合 `PUT` 200。同一 MCP 会话 `list_sources`：空 → `qa-a` → `qa-a,qa-b` → `qa-b`，撤权后 `list_tables` 返回 `TOOL_NOT_GRANTED`，三次管理写请求恰为 3 个 `PUT`。
- 证据：本地 `.plan/ego-t13-t18-20261122/` 的 `t17-fixed-picker.json`、`t17-fixed-flow.json`、`round-03-add-grants.mjs`。外部写调用数：0（仅隔离状态库）。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
