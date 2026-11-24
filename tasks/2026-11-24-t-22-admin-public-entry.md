# T-22 管理面与 MCP 公网 HTTPS 入口

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整（用户可见行为、对外暴露面、SPEC 基线变更） |
| 状态 | 🟡 待验收 |
| 依赖 | T-21（交付流水线与部署单元） |
| AC | AC2-10（修订后的部署与管理隔离） |
| 任务分支 | `feat/zcode/t-22-admin-public-entry`（合并 PR #9）、`diag/zcode/t-21-secrets`（CI/CD 拆分所在，PR #7 已并入） |
| 合并目标 | `develop` → `main` |
| worktree | 无 |
| 集成分支提交 | `86c61ab`、`e12854d`、`754645f`、`2caa930` 及本次收尾提交 |

## 上下文包

- 权威文件：[SPEC.md](../SPEC.md) AC2-10（本任务修订）、[部署手册](../docs/deploy.md) §5/§7、[CHANGELOG.md](../CHANGELOG.md)。
- 关键契约：公网只暴露 443（Caddy，TLS 终止）；mcp-db 两个端口保持仅宿主回环；应用按成对的 `X-Forwarded-Proto: https` + `X-Forwarded-Host` 校验边界与 Origin；HTTPS 下会话 cookie 带 `Secure`。
- 不可变量：直连非回环 Host 必须拒绝；代理头不成对/链式视为不存在；用户决策为 IP 直访自签证书、8787 与 8788 都经代理开放、不加代理层认证。
- 入口命令：`bash scripts/verify.sh fast`；公网实测见第 4 节。
- 证据位置：GitHub Actions 记录（公开）+ 本文件记录的服务器实测输出（脱敏）。

## 1. 范围

做什么：

- SPEC AC2-10 从「首版仅本机」修订为「默认回环 + 可选受信 TLS 反代公网入口」。
- `src/transport/admin.rs`：新增 `forwarded_origin`（成对代理头才信任），`request_boundary` 按 https+转发 host 校验，HTTPS 下 `set_cookie_header`/`clear_cookie_header` 附加 `Secure`；新增 4 个边界测试。
- `deploy/`：compose 新增 `caddy` 服务（443 → /admin→8788、/mcp→8787），新增 `Caddyfile`（`tls issuer internal` + `on_demand`，`header_up X-Forwarded-Host {http.request.host}`）；cd.yml 同步清单加入 Caddyfile。
- `docs/deploy.md` §5/§7、`CHANGELOG.md` 同步。

不做什么：

- 不做代理层认证（用户选择仅用管理台登录凭证）。
- 不做域名与 Let's Encrypt（用户无域名；换域名只是 Caddyfile 一行改动）。
- MCP 端口仍经代理开放，但只靠应用层 Bearer 认证（用户确认两个都开）。

## 2. 验收例子

1. 经 Caddy 的 HTTPS 请求（成对转发头）：`/admin/login` 200；带有效令牌+challenge 的登录 200，`Set-Cookie` 含 `Secure; HttpOnly; SameSite=Strict`。
2. 直连 8787/8788 的非回环 Host 请求被 403 拒绝（回环强制不因代理存在而放松）。
3. 伪造/不成对的 `X-Forwarded-*`：被 Caddy 覆盖或不被应用信任，不能绕过边界。
4. 443 是唯一公网端口；`docker port mcp-db` 仍只有 127.0.0.1 映射。
5. 主机按 IP 无 SNI 访问时 TLS 握手成功（on_demand 自签），浏览器可打开登录页。

## 3. 计划与风险

- 设计依据：SPEC §2 部署生命周期（修订后）、`docs/PRD.md`；用户 2026-11-24 确认「IP 访问、两个都开、不加认证层」。
- 对抗性检查：单边伪造 X-Forwarded-Host（无 Proto 或反向）不生效；Origin 与转发 host 不匹配拒绝；Caddy 覆盖 X-Forwarded-Host 使客户端伪造失效；443 之外的端口仍只回环。
- 奥卡姆检查：不引入 OAuth/代理层密码；不预签域名证书；不改 MCP 认证模型。
- 实测修掉的两处配置缺陷：
  1. `{host}` 在 `header_up` 中解析为上游地址（mcp-db:8788）→ 改 `{http.request.host}`；
  2. `https://:443` 按 SNI 签发证书，IP 直连无 SNI 时 TLS 握手 internal error → `tls issuer internal` + `on_demand`。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 未验证 |
| --- | --- | --- | --- | --- | --- |
| CI | 2 | PR #9 的 verify（run 45324965487） | GitHub Actions | fmt/clippy/test 全绿，含 4 个新边界测试、镜像构建与冒烟 | — |
| 应用边界（真实容器） | 2、3 | `docker run … curl http://mcp-db:8788/admin/api/login`（带/不带转发头各变体） | 目标主机 compose 网络 | 带 `X-Forwarded-Proto: https` + `X-Forwarded-Host: <ip>` + `Origin: https://<ip>` → 进入 CSRF 校验（"登录页面已过期"）；无转发头直连 → 403 回环强制；Origin 不匹配 → 403 | — |
| TLS 握手 | 5 | `curl -sk https://127.0.0.1/admin/login`（on_demand 修复后） | 目标主机 | 200；日志显示对 127.0.0.1/容器 IP 按需签发证书 | — |
| 登录端到端 | 1 | 生成一次性令牌 → GET 挑战 → POST `/admin/api/login`（带 `X-Mcpdb-Csrf`） | 目标主机经 Caddy | HTTP 200；`Set-Cookie: mcpdb_admin=…; SameSite=Strict; HttpOnly; Secure`；`/admin/api/session` 返回 `authenticated:true` | — |
| 伪造防护 | 3 | 客户端自带 `X-Forwarded-Host: evil` 经 Caddy | 目标主机 | 403（代理覆盖该头） | — |
| 端口面 | 4 | `docker ps` / `docker port mcp-db` | 目标主机 | 443 仅 Caddy；8787/8788 仅 127.0.0.1 | — |
| 公网访问 | 5 | 浏览器打开 `https://<ip>/admin` | 用户侧 | 未执行 | 🔴 云防火墙放行 443 后由用户验收 |
| 手工配置收口 | — | 合并 release PR 后 cd 自动同步 Caddyfile 并重启 caddy | cd 流水线 | 未执行 | 🔴 随 release PR 验证 |

外部写：GitHub 推送 2 次、开 PR 1 次、合并 1 次、重跑 cd 1 次；服务器：scp Caddyfile×2、compose restart caddy×3、生成一次性登录令牌 3 枚（全部消费或过期）。

## 5. 交付

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未验收 | 浏览器经公网访问 `https://<ip>/admin` 并登录 | 443 需在腾讯云控制台放行，且证书警告需用户本人确认 | 影响 AC2-10 公网部分；不处理则公网入口只有服务器侧证据 | 放行 443 → 访问 → 信任证书警告 → 用一次性令牌登录 | 否 |
| 🔴 未验收 | release PR 合并后 cd 收口手工配置 | 手工 scp/restart 的 Caddy 配置需由流水线重新同步 | 不处理则服务器配置属于仓库外漂移 | 合并 release PR，确认 cd 绿且 Caddyfile 时间戳更新 | 否 |
