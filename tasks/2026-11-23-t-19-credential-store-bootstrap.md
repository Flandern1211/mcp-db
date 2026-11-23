# T-19 修复全新部署无法初始化凭证库

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：凭证与启动路径，跨部署边界 |
| 状态 | 🟢 已完成 |
| 依赖 | 无 |
| AC | AC-1～AC-3（见第 2 节） |
| 基线 | `main` / `develop` 的 `59abbcc` |
| 任务分支 | `docs/zcode/readme-refresh`（与 README 重写同分支，分提交） |
| 合并目标 | `develop`，再同步 `main` |
| worktree | 无（主工作区） |
| 集成提交 | 见 `TASK.md` 的 T-19 行 |

## 1. 范围

做什么：

- 服务启动时若已配置 `MCPDB_CREDENTIAL_STORE` 与 `MCPDB_MASTER_KEY_FILE`，而凭证库文件不存在，则创建空凭证库后继续启动。
- 保持失败关闭：主密钥缺失、不可读或格式非法时启动失败，且不留下空库。
- 回归测试覆盖「空目录首次启动」「密钥非法不创建库」「打开已有库仍可解密」。

不做什么：

- 不改加密格式、主密钥解析、路径校验、`open` 的严格语义和 legacy 迁移流程。
- 不新增 CLI 子命令，不改管理接口契约。
- 不清理任何真实容器、卷或镜像。

## 2. 验收例子

1. 干净目录 + 合法主密钥 + 不存在的凭证库路径：服务正常启动，`/data/credentials.enc` 被创建，管理页可保存 DSN。
2. 主密钥文件缺失或内容非法：启动以 `credential init failed:` 失败，且不创建凭证库文件。
3. 已存在凭证库 + 正确主密钥：启动后仍能解密并读取已有条目；主密钥换成别的密钥时失败关闭。

## 3. 计划与风险

- Bug 根因检查：失败证据 `mcp-db: credential init failed: credential reference cannot be resolved` → `src/main.rs` 启动分支调用 `EncryptedFileResolver::open`，该函数在文件缺失时返回 `CredentialUnavailable`；同模块已有的 `open_or_create` 只在 legacy 迁移里被调用 → 修复点：启动改用 `open_or_create`，并用 `open_credential_resolver` 统一错误前缀。
- 设计依据：[`SPEC.md`](../SPEC.md) §2 部署与运行生命周期、§5.2 数据模型与凭证；`docs/PRD.md` §5 产品原则「管理面不对 agent 开放」「数据库权限是最终边界」。
- 步骤：改启动分支 → 抽 `open_credential_resolver` → 单测与集成测试 → `verify.sh fast`/`full` → 干净容器真实入口验证。
- 拆分结论：单任务；README 重写是本修复的下游文档，单独提交。
- 并行判定：串行。只碰 `src/main.rs`、`tests/credentials.rs`、文档。
- 风险：`open_or_create` 先判断文件是否存在，存在并发窗口；首版承诺单写者，不扩大范围。主密钥错误时的错误文案仍统一为 `credential init failed: credential reference cannot be resolved`，不区分「缺文件/错密钥」。
- 第一性原理检查：目标是「全新部署能建立第一条数据库连接」；不可变约束是「凭证只以密文落盘、密钥缺失必须失败」；关键假设是「空库不影响后续加密写入」；推导结论是创建空库是满足两者最小的改动。
- 对抗性审查：若启动时静默跳过凭证库，会让管理页报错更晚且更难诊断，故不采用；若自动生成主密钥，会把密钥混进数据卷，破坏「密钥由管理员保管」的前提，故拒绝。
- 奥卡姆检查：复用已有 `open_or_create`，不新增子命令、不复刻加密逻辑。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元 | AC-1、AC-2 | `cargo test --locked`（经 `scripts/verify.sh fast`） | 固定 Rust 1.89 容器 | 新增 2 条 `src/main.rs` 单测：空目录创建库并可存取、缺失/非法密钥失败且不建库 | 无 |
| 集成 | AC-1～AC-3 | `cargo test --locked --test credentials` | 同上 | 新增 2 条：`open_or_create` 创建空库后写入读回；缺失、非法、错误密钥均失败 | 无 |
| 真实入口 | AC-1、AC-3 | 干净 `--tmpfs /data` + 新主密钥启动 `mcp-db:dev`；管理台登录、建源、存 DSN、测试连接、勾选工具、签发令牌、MCP `list_sources`/`list_tables`/`query_sql`、未授权 `mutate_sql` 拒绝 | Windows Git Bash + Docker Desktop；临时 PostgreSQL 16 容器；临时容器与卷在验证后删除 | 健康检查 400ms 内 OK；`credentials.enc` 与 `state.db` 自动生成；测试连接 `app / ro / PostgreSQL 16.16 (463 ms)`；`query_sql` 返回 `hello from readme`；`mutate_sql` 返回 `TOOL_NOT_GRANTED` | 无 |
| 回归 | AC-1～AC-3 | `bash scripts/verify.sh full` | 同上 | fmt、clippy、test、release build、`docker build` 全部 OK；Rust 143 passed / 0 failed / 11 ignored | 无 |

外部写调用数：0（仅本机临时 PostgreSQL 与隔离状态库）。

证据位置：本机 `.plan/readme-refresh-20261123/`（私有，不入库）；提交正文记录命令与结论。

## 5. 交付

- 提交与 `TASK.md`：见 `TASK.md` 的 T-19 行；状态 🟢 已完成，未关闭项 0。
- 已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
