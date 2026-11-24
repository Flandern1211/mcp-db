## Context

T-07 已合入 `develop`，T-08（AC2-17～21）依赖的 T-04～T-07 全部完成。探索确认：`Tool::CreateDatabase`、grant/route 强制 `approval`、审批状态机、来源父级字段都已存在，但**没有任何建库执行路径**——没有 `create_database` handler、没有 provision 执行器、没有配额预留表、没有归档/恢复方法、审批摘要也不绑定建库规格。本轮按你的选择实施 T-08，三阶段提交、每阶段独立验收，并启用 ponytail。

已确认的决策：
- 新库使用**每库独立角色**：provision 账号建库，另建 `_ro`（仅 SELECT）与 `_rw`（库 owner）角色，DSN 写入现有加密凭证库。
- 交付拆三阶段；归档/恢复与管理员入口留在第三阶段。

## 设计要点

**建库规格由服务端生成，存入审批而不是重算。** `create_database` 只接受 `source`、可选 `label`、`approval_id`。规格（数据库名 `mcpdb_<np|prod>_<10位随机>`、环境继承父 source、结果 tool 模板、角色名）序列化为固定字段顺序的 JSON 写入 `approval_requests.params_json`，摘要（`ApprovalDraft::digest` 已覆盖 `params_json`）因此绑定规格。批准后重试**凭 approval id + token 复用已存规格**，不重新生成随机名；这是与 SQL 重试“客户端重发同一请求”不同的必要偏差，会在 SPEC 3.5 写明。

**配额以持久预留为准。** 新表 `quota_reservations`（token、父 source、库名、approval、状态 `reserved/committed/released/quarantined`）。审批前做只读检查；`claim` 时在**同一 SQLite 事务**内完成“执行前重验 + 数量/频率预留 + 审批一次性消费”，因此并发只有一个能拿到预留。数量默认 5（归档子 source 不计），频率默认 60s 1 次；预留计入频率窗口。成功登记后 `committed`；证明未创建则 `released`；跨边界部分成功/结果未知则 `quarantined`，阻塞该 token×父 source 后续建库，重启时 `reserved → quarantined`。

**provision 身份严格隔离。** 新增 `src/provision.rs`：`Provisioner` trait + `PostgresProvisioner`，直接用 provision profile 凭证连接（10s 连接/30s 执行期限，`NoTls`），**不经过** `ProfileConnections`（其 provision 拒绝保持不动）。只执行服务端生成并校验过的标识符：`CREATE DATABASE` → `CREATE ROLE _ro/_rw`（随机 base64url 口令）→ `ALTER DATABASE OWNER TO _rw` → `REVOKE CONNECT FROM PUBLIC` → schema 与 default privileges。任何 DSN/口令不进入审批 JSON、审计或日志。
- 已发出 DDL 但连接中断/超时 → `ExecutionUnknown` + 隔离；服务器返回 SQL 错误（语句原子）→ 证明未创建，释放。
- 创建成功但角色/凭证/登记任一步失败 → 隔离预留并返回明确的未知/部分成功，不自动清理。

**凭证写入。** `EncryptedFileResolver` 增加 `upsert(reference, dsn)`：进程内 `Mutex` + 更新整表后原子写回（临时文件 + rename，unix 0600），写后立即可 `resolve`。新增 `CredentialSink` trait；未配置凭证库时 provisioner 拒绝建库。单实例单写者，跨进程并发不做承诺，写入 SPEC。

**新库登记事务化。** 新增 `Store::register_provisioned_source`：source（id = 库名，父级、创建 token/agent/来源）+ query profile/routes（+ 非生产按模板建 mutate/schema）+ 创建 token 的结果 grants + 预留 `committed` 在一个 `unchecked_transaction` 内完成；先落凭证、后登记，登记失败即隔离。结果 grants 永不含 `create_database`，生产环境模板只有只读 tools。

**管理入口（第三阶段，最小集合）。** T-09 才做 Web 与完整 CLI，但 AC2-21 需要可操作的 admin 路径：本轮加 `mcp-db approvals|approve|deny|archive-source|restore-source|reservations|settle-reservation` 的最小 CLI（直连状态库、仅容器内本地执行）。归档在同一事务内改状态、失效该 source 的 pending/approved 审批并审计；恢复校验 profiles/grants 与配额后再置 active。

## 实施步骤

**阶段 1：状态与配额基础（提交 1）**
1. `src/state/store.rs`：`SCHEMA_VERSION = 3` + `MIGRATION_3`（`quota_reservations` 表与索引）；`reserve_provision`、`settle_reservation`、`list_quarantined_reservations`、`active_provisioned_count`（排除 archived）；扩展 `recover_after_restart` 把 `reserved` 转 `quarantined`。
2. `src/provision.rs`：`ProvisionSpec`（固定字段顺序、canonical JSON）、`ProvisionLimits`、标识符/口令生成与校验（复用 `validate_source_id` 思路，只放行服务端生成值）。
3. `src/config.rs`：`MCPDB_PROVISION_MAX_ACTIVE`（默认 5，1～100）、`MCPDB_PROVISION_MIN_INTERVAL_MS`（默认 60000，1～86400000）、`MCPDB_PROVISION_RESULT_TOOLS`（默认 `list_tables,describe_table,query_sql,explain_query`，只读集合，非法值拒绝）。
4. 测试 `tests/provisioning.rs`：预留/结算/隔离状态机、并发只成功一个、频率窗口、归档不计数量、重启隔离、规格 JSON 稳定与摘要变化。

**阶段 2：建库闭环（提交 2）**
5. `src/credentials.rs`：`CredentialSink` + `EncryptedFileResolver::upsert`（原子写、0600、热更新），测试覆盖并发读、损坏不回退。
6. `src/provision.rs`：`PostgresProvisioner` 与 DSN 组装（仅 TCP host，Unix socket 明确拒绝）；完整建库序列与错误分类。
7. `src/state/store.rs`：`register_provisioned_source` 单事务登记（含复用式校验），`count`/`reserve` 与审批消费组合方法 `claim_provision`。
8. `src/mcp.rs`：`CreateDatabaseArgs`、`create_database` handler（授权 → 配额预检 → 规格 → admin 审批；`approval_id` 路径取已存规格 claim → 执行 → 登记）；`execute_claimed` 按 `draft.tool` 分派；取消包装改用包含 `CreateDatabase` 的保守谓词；`AppState.provisioner` 与 `main.rs`/`http.rs` 装配（无凭证时 `UnavailableProvisioner` 返回 `PROVISION_UNAVAILABLE`）。
9. 测试：`tests/http_mcp.rs` 增 mock provisioner 的申请/拒绝/批准后一次性执行/并发拒绝；`tests/provision_postgres.rs`（ignored，`MCPDB_TEST_PG_ADMIN_DSN` 需 CREATEDB+CREATEROLE）验证真实建库、`_ro` 写入被原生拒绝、`_rw` 可建表、登记后 query/mutate tool 可用、失败与未知路径隔离。

**阶段 3：生命周期与文档（提交 3）**
10. `Store::archive_source` / `restore_source`（单事务 + 审批失效 + 审计 + 配额复核）、`settle_quarantined_reservation`（admin 释放或确认）。
11. `src/main.rs`：最小 admin 子命令（见上），`print_help` 同步。
12. 集成测试：归档阻止调用并使待批失效且不删物理库；恢复超配额被拒；隔离预留结算后恢复创建；重启后旧批准不重放。
13. 文档：`SPEC.md` 3.5/5.2（命名、双角色、凭证写入、模板、配额参数、隔离语义）、`CHANGELOG.md` 一条基线记录、`docs/PRD.md` 第 8 节把建库命名/环境/模板/账号策略标为已确定；任务文件写三阶段证据。

## 验证

- 每阶段：`cargo fmt --check`、`cargo test --locked`、`cargo clippy --locked --all-targets --all-features -- -D warnings`（Docker `rust:1.89-slim-bookworm`）。
- 阶段 2/3 真实集成：隔离 PostgreSQL 16 容器 + `CREATEDB`/`CREATEROLE` 测试账号，`cargo test --locked --test provision_postgres -- --ignored`；测试自行清理创建的库与角色。
- 收口：`cargo build --release --locked`、`docker build`、容器非 root/健康检查；三次本地提交，最后合入 `develop` 并回写 `TASK.md`；不推送。

## 风险与取舍

- **部分成功不可自动回滚**：建库跨外部与本地边界，只能在隔离预留后由 admin 核对；这是 SPEC 要求，测试覆盖“证明未创建/未知/部分成功”三类。
- **凭证写入是新增能力**：只支持单实例单写者与原子覆盖，不做跨进程锁；写入失败按未知处理。
- **随机库名重试语义变化**：建库重试不重发参数，改为按 approval id 取服务端规格；已在计划中说明并会写入 SPEC。
- **T-05 遗留**：SELECT 副作用绕过属既有问题，不在本轮修复。