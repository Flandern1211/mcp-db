# T-08 实现 PostgreSQL 建库、配额与 source 生命周期

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：真实外部建库、凭据、并发配额和生命周期 |
| 状态 | 🟢 已完成 |
| 依赖 | T-04～T-07 |
| AC | AC2-17～AC2-21 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-08-provisioning-lifecycle` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | `6ddc66f` |

## 1. 范围

- 开工前确定 PostgreSQL 最低版本、账号配置/补偿、容器内凭证安全存储方案（系统密钥环或等价加密存储 + 外部主密钥）与人工核对入口。
- `create_database` 精确 grant + provision route + 每次 admin 审批；服务端生成数据库名称。
- 原子数量/频率预留、未知结果隔离、就绪后登记 grants、归档/恢复与配额复核。
- 不开放删库、任意管理 SQL、递归建库或 provision 凭证进入普通 SQL。

## 2. 验收例子

1. 缺 grant 或 provision 映射在审批前拒绝，provision profile 不能进入普通 SQL。
2. 并发申请只有配额内请求获预留；未知结果阻塞同 token × 父 source 后续创建。
3. 新 source 只在全部就绪后可见，创建者获明确 grants，其他 token 不可见且不继承建库能力。
4. 归档不删物理库，恢复须重新满足 profile/grant/配额。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：SPEC 3.5、3.6、5；实现前完成 PostgreSQL 建库/凭据 spike 并更新基线决定。
- 步骤：provision adapter → reservation 状态机 → approval 绑定 → 登记/隔离 → 生命周期 → 真实集成。
- 拆分结论：外部副作用与本地登记有独立未知状态，单独任务最小可回滚。
- 并行判定：依赖 T-04～T-07 契约，串行。
- 风险：部分成功和凭据泄露；未知结果不自动补偿，凭据只存安全引用。
- 第一性原理检查：agent 只能申请，admin 决定一次，服务端控制目标与身份。
- 对抗性审查：覆盖并发、断连、登记失败、递归与恢复超额。
- 奥卡姆检查：不做自动删库、TTL、存储配额或 workspace。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元/状态库（阶段 1） | AC2-19、AC2-21 | `cargo test --locked --test provisioning` | 临时 SQLite | 配额预留/结算/隔离、成功频率按结算时间、归档不计数量、重启隔离、admin 打开不触发恢复均通过 | 无 |
| MCP 工具链（阶段 2/3） | AC2-17、AC2-18、AC2-20 | `cargo test --locked --test http_mcp` | Mock provisioner + 临时 SQLite | 缺 grant/route 在审批前拒绝；审批后一次性执行并登记；创建者获结果 grants、其他 token 不可见；配额/隔离阻止；未开始释放、未知/登记失败隔离；取消保守处理 | 无 |
| 凭证与登记 | AC2-18、AC2-20 | `cargo test --locked --test credentials --test provisioning` | 临时加密凭证库 | upsert 原子替换并热加载、并发读一致、非法引用拒绝；登记事务回滚；生产写 grant 拒绝；provision 命名空间隔离 | 无 |
| PostgreSQL 真实集成 | AC2-17～AC2-20 | `cargo test --locked --test provision_postgres -- --ignored`；`cargo test --locked --test http_mcp create_database_end_to_end_with_real_postgres -- --ignored` | 隔离 PostgreSQL 16；`CREATEDB`/`CREATEROLE` 测试账号 | 2 passed：建库与双角色、`_ro` 原生拒绝写入/DDL、`_rw` 建表插入、默认权限可读、登记后 query/metadata 可用；MCP 全链路（申请→admin 批准→建库→登记→同会话发现→查询→其他 token 不可见）通过；测试清理数据库与角色 | 无 |
| 构建与静态检查 | 工程基线 | `cargo fmt --check`；`cargo test --locked`；clippy；`cargo build --release --locked`；`docker build`；容器健康检查与 admin CLI | Docker Desktop Linux | 全量 87 passed, 11 ignored；clippy 严格通过；镜像构建、非 root、healthcheck、`approvals`/`reservations` 空队列输出通过 | 无 |

外部写调用数：真实测试创建并删除 1 个数据库、2 个角色；无其他外部写入。
证据位置：本任务文件；真实集成在本地隔离容器执行，未记录连接串或口令。
环境边界：`_rw` 仅获库内对象创建权限，数据库 owner 保留给 provision 账号；PostgreSQL 最低支持版本仍为 PRD 未决项，本任务仅在 16 验证。

## 5. 交付

- 提交与 `TASK.md`：`d30cba5`、`31e9651`，合入 `6ddc66f`；🟢 已完成，未关闭项 0。已接受风险：无。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 无 | — | — | — | — | — |
