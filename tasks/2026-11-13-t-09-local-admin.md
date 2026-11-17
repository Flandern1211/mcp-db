# T-09 实现首版本机 CLI 与 Web 管理面

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：管理会话、审批、生命周期与本地网络边界 |
| 状态 | 🟢 已完成 |
| 依赖 | T-02、T-06、T-08 |
| AC | AC2-10（首版本机范围）、AC2-21、AC2-22 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-09-local-admin` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | `7458d3f` |

## 1. 范围

- 开工前确定一次性登录令牌有效期、admin cookie、CSRF、Web 信息架构和管理端口发布方式。
- 首版管理面只允许本机：管理监听使用独立端口，Docker 只发布到宿主回环地址；CLI 生成一次性登录令牌。
- admin 会话 cookie 使用 HttpOnly、SameSite=Strict；管理写操作校验 CSRF；agent token 不能访问管理面。
- 实现 token/profile/route/grant、审批、审计、归档恢复所需最小 CLI 和 Web 管理面。
- 不实现跨主机访问、TLS 终止、受信代理、Origin allowlist 或多管理员角色；这些归后续远程管理任务。

## 2. 验收例子

1. Docker 只发布管理端口到宿主回环地址；CLI 生成的一次性令牌只能登录一次，过期/重复使用失败。
2. 错误来源的 admin 写请求、缺 CSRF 的写请求和 agent token 均被拒绝。
3. 管理员可通过 CLI 或 Web 审批、查看审计、管理 grants 和归档/恢复 source；同一决定不重复执行。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 1、2、4、6 与 PRD 首版管理边界。
- 步骤：一次性令牌 → 管理监听/来源校验 → session/CSRF → CLI 与 management handlers → 审计/审批联动 → 容器 E2E。
- 拆分结论：远程 TLS/代理/Origin 需要独立网络验收，不并入首版本机管理。
- 并行判定：依赖控制面服务契约，串行。
- 风险：把“本机端口”误当安全边界；仍使用一次性令牌、来源限制、session/CSRF 和 agent/admin 隔离。
- 第一性原理检查：管理入口不因容器端口发布方式降低认证强度。
- 对抗性审查：覆盖重复登录令牌、伪造来源、跨站请求和 agent/admin token 混用。
- 奥卡姆检查：不做组织、多角色、外部身份提供商或远程控制面。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元/模块集成 | AC2-10、AC2-21 | `cargo test --locked --test admin_http` | axum router + 临时 SQLite | 8 passed：令牌单次/过期、缺/错 CSRF、错误 Origin、agent Bearer 拒绝、重复批准不执行、归档后 MCP 不可见、隔离预留结算、审计页无凭证 | 无 |
| CLI 与审计 | AC2-09、AC2-21 | `cargo test --locked --bin mcp-db`；容器内 `mcp-db audit` | 临时状态库 | 21+3 passed：source/profile/route/token/grant 命令可用；7 类管理变更写 `admin_*` 审计且不含令牌明文/hash；审计倒序可查 | 无 |
| 容器 E2E | AC2-10（本机） | `docker run -p 127.0.0.1:28787:8787 -p 127.0.0.1:28788:8788`；curl 登录/重放/CSRF/Origin/agent token | 本地 Docker + 非 root 容器 | `docker port` 仅回环；user 10001、health healthy；login 200/303、dashboard 200、重放 401、缺 CSRF 403、错误 Origin 403、agent Bearer 401 | 无 |
| 构建与静态检查 | 工程基线 | `cargo fmt --check`；`cargo test --locked`；clippy；`cargo build --release --locked`；`docker build` | Docker Desktop Linux | 104 passed, 11 ignored；clippy 严格通过；release 与镜像构建通过 | 无 |

外部写调用数：仅本地容器内状态库与管理操作；无外部系统写入。
证据位置：本任务文件与本地容器冒烟记录（未记录登录令牌、agent token 明文或 cookie 值）。

## 5. 交付

- 提交与 `TASK.md`：`2405712`，合入 `7458d3f`；🟢 已完成，未关闭项 0。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
