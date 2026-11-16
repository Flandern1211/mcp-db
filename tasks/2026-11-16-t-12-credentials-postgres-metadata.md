# T-12 实现凭证解析与 PostgreSQL 元数据 provider

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：凭证安全存储、真实数据库连接与 MCP 元数据工具 |
| 状态 | 🟢 已完成 |
| 依赖 | T-02 |
| AC | AC2-01、AC2-09、AC2-11、AC2-12 的凭证与元数据基础 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-12-credentials-postgres-metadata` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | 待合入 |

## 1. 范围

做什么：

- 定义 `CredentialResolver` seam，把 `keyring:<account>` 引用解析为数据库连接凭证。
- 首版提供加密凭证文件实现：密文存储在状态目录，主密钥由外部 Docker secret 文件提供；不把明文写入配置、状态库、日志或审计。
- 实现 PostgreSQL `MetadataProvider`：`list_tables`、`describe_table`；`explain` 在 T-05 前失败关闭。
- 用 `tokio-postgres` 建立最小只读连接；连接池、超时和 profile 上限留给 T-07。
- 接入配置、启动装配和受控真实 PostgreSQL 集成入口。

不做什么：

- 不实现 `explain_query` 的 SQL 策略、`query_sql`/`mutate_sql`/`execute_ddl`；这些归 T-05。
- 不实现连接池、statement timeout、rate limit；归 T-07。
- 不实现建库账号生成、配额或 provisioning 凭证；归 T-08。
- 不提供管理 CLI/Web 凭证编辑；T-09 接入管理面，T-12 只提供存储和加载 seam。
- 不把 DSN 或密码放进环境变量、TOML、状态库或镜像。

## 2. 验收例子

1. 加密凭证文件可在给定主密钥下解析 `keyring:<account>`；缺少主密钥、错误密钥或未知引用时失败关闭；文件字节不含明文 DSN/密码。
2. 受控 PostgreSQL 只读实例上，MCP `list_tables` 和 `describe_table` 成功返回结构；未授权 source 的调用在建立连接前拒绝，provider 调用数为 0。
3. profile 停用、source 归档、route 缺失或凭证引用非法时，在连接前拒绝。
4. 容器以非 root 启动，通过挂载的主密钥文件加载凭证；镜像、配置和日志不含明文凭证。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 3.1、3.2、5.2；T-04 的 `MetadataProvider` 契约；PRD 首版 PostgreSQL 范围。
- 步骤：加密存储格式 → `CredentialResolver` → `PostgresMetadata` → 配置/启动装配 → 受控集成 → MCP 端到端。
- 拆分结论：凭证存储和真实 provider 是 T-04 的前置依赖；`explain` 策略独立归 T-05。
- 并行判定：串行；T-04 完成依赖本任务，T-05 在 T-04 后实施。
- 风险：容器内无系统密钥环；用加密文件 + Docker secret 主密钥实现等价安全存储；主密钥轮换和导入 CLI 由后续管理任务补齐。
- 第一性原理检查：agent 和日志永远看不到明文凭证；数据库连接身份只由管理员配置的 profile 决定。
- 对抗性审查：错误密钥、缺失引用、密文篡改、非 root 文件权限、未授权 source 连接数、日志泄露。
- 奥卡姆检查：不引入多后端抽象；先做一个加密文件 resolver，保留 trait 供后续 keyring 实现。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元 | 凭证存储 | `cargo test --test credentials`；两个 ignored fixture 测试单独运行 | 临时目录 + 虚构 DSN | 3 default passed；seal/open fixture passed；密文不含明文 DSN/密码；错误密钥、缺失密钥、篡改均失败关闭 | 无 |
| 模块集成 | AC2-12 | `cargo test --test http_mcp` | Mock metadata | 3 default passed：未授权 source 不调用 provider；工具错误码可区分 | 无 |
| 真实集成 | AC2-12 | `cargo test --test postgres_metadata -- --ignored`；`cargo test --test http_mcp -- --ignored` | 本地临时 PostgreSQL 16 + 只读账号 | 2 passed：真实 `list_tables`/`describe_table`；加密凭证经 MCP 查询真实 `users` 表和列；只读账号 INSERT 被数据库拒绝 | 无 |
| 容器 | AC2-12 | `docker build`；带挂载加密凭证和主密钥启动；缺失凭证启动 | Docker Desktop Linux 容器 | 构建通过；`user=10001:10001`、`health=healthy`；缺失凭证 `exit=1` | 无 |

外部写调用数：0 个外部业务写入；本地测试数据库仅插入 1 条夹具，只读账号按预期被拒绝写入。证据位置：本任务文件和本地提交正文（仓库可见）。

## 5. 交付

- 提交与 `TASK.md`：`c790f64`；待合入 `develop` 并回写状态。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
