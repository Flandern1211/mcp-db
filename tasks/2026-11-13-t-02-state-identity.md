# T-02 建立 Rust 工程与 AC2 状态/身份基础

| 项目 | 内容 |
| --- | --- |
| 档位 | 完整：持久数据模型、身份、跨模块安全边界与工程基线 |
| 状态 | 未开始 |
| 依赖 | 无 |
| AC | AC2-01、AC2-09、AC2-11、AC2-22 的持久化基础 |
| 基线 | `57041e4` |
| 任务分支 | `feat/zcode/t-02-state-identity` |
| 合并目标 | `develop` |
| worktree | 未分配 |
| 集成分支提交 | — |

## 1. 范围

做什么：

- 建立 Rust 工程模块边界、配置入口、状态库连接和首个版本化 schema migration。
- 建立 source、connection profile、tool route、token、grant 的持久模型。
- token 只保存哈希和识别信息，支持撤销与重启后解析。
- 在写入边界拒绝未知 tool/profile、用途错配、非法凭据引用、production 普通写 grant 和 provision 越界路由。
- 保留 legacy v1 的 approvals/audit 历史，不让旧记录产生新权限。
- 建立 Rust 单元/集成测试和 Docker 本地运行入口。

不做什么：

- 不实现 MCP 工具执行路径、SQL parser、审批消费、建库、Web 管理页或迁移 cutover。
- 不引入未经 spike 验证的 MCP、SQL parser 或数据库驱动 crate。
- 不把新旧权限合并读取；旧配置迁移归 T-03，运行时授权归 T-04。

## 2. 验收例子

1. `cargo fmt --check`、clippy、test、release build 和 Docker 健康检查通过；容器以非 root 启动。
2. 签发 token 后状态库只有哈希；未知和撤销 token 被区分拒绝，重启后状态一致。
3. source、多个同用途 profiles、routes 和 grants 重启后仍存在；production 普通写 grant、用途错配 route、普通 tool 指向 provision profile 均保存失败。
4. 从 legacy v1 状态库启动升级后，旧 approvals/audit 仍可读，schema 有明确版本，且没有凭空生成 token/grant。

## 3. 计划与风险

- Bug 根因检查：不适用。
- 设计依据：`docs/PRD.md`、`SPEC.md` 3.1、3.2、5.2 与 AC2-01/09/11/22。
- 步骤：选择 SQLite crate 与配置格式 → 模块边界 → 版本化 schema → 领域错误与模型写入校验 → 持久 token 解析/撤销 → 测试和容器入口。
- 拆分结论：本任务只交付后续任务共享的 Rust 工程、状态与身份地基；运行时切换和旧配置 cutover 可独立验收，故拆至 T-03/T-04。
- 并行判定：串行；T-03/T-04 依赖本任务 schema 契约。
- 风险：过早在工程骨架中锁定 MCP/DB crate；缓解为本任务只加状态与配置所需依赖。
- 第一性原理检查：目标是建立默认拒绝且可持久复核的授权事实；不可变约束是不存明文 token/凭证且不扩大旧权限。
- 对抗性审查：即使直接写入越权记录，也必须由数据库约束或存储层校验拒绝；重启不能恢复撤销 token。
- 奥卡姆检查：不新增管理 API、凭据后端抽象、缓存或兼容读取层。

## 4. 测试与证据

| 层级 | AC | 命令或步骤 | 环境 | 结果 | 🔴 未验证 |
| --- | --- | --- | --- | --- | --- |
| 单元/模块集成 | AC2-01、09、11、22（基础） | `cargo test` | 本地临时目录/SQLite | 未开始 | 全部 |
| 构建/容器 | 工程基线 | `cargo fmt --check`；`cargo clippy --all-targets --all-features -- -D warnings`；`cargo build --release --locked`；`docker build`；容器健康检查 | 本地 Rust + Docker | 未开始 | 全部 |

外部写调用数：0。

证据位置：本任务文件和本地提交正文（仓库可见）。

## 5. 交付

- 提交与 `TASK.md`：未开始。已接受风险：无。
- 重规划记录：本任务最初按 Bun/TypeScript 基线创建；2026-11-16 用户确认核心服务改为 Rust，原分支无独有提交，原 Bun 验证命令未执行，任务号继续表示同一产品结果。

| 类型 | 项目 | 原因 | 影响 | 开发可以做什么 | 阻塞后续 |
| --- | --- | --- | --- | --- | --- |
| 🔴 未测试 | 全部 AC | 任务未开始，尚未执行任何验证 | 无实现证据；不能标完成 | 按范围实现并运行第 4 节命令；全部通过后关闭 | 否 |
