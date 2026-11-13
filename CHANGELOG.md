# 变更记录

## 2026-11-13

- 授权模型由“仅按 source 三档模式”扩展为 **token MCP tool 白名单 × token source 模式 × source 自身策略**；工具不可见不替代服务端调用时鉴权。
- 生产 source 的普通 SQL 对 agent 始终只读；普通写入继续按 `readonly`、`readwrite`、`approval` 处理，高危 SQL 即使在 `readwrite` 下也须由 admin 逐次审批。SQL 策略按方言 fail-closed 解析，拒绝多语句、解析失败和未知语句。
- 新增专用 `create_database` tool：source 必须由 admin 显式开启、token 必须同时具备 tool 和 source 权限、通过原子配额预留且每次由 admin 人工批准；建库凭证与普通连接池隔离。
- 通用 SQL 禁止 `CREATE DATABASE`、`DROP DATABASE`、`GRANT`、`REVOKE`；`DROP DATABASE` 不向 agent 开放。
- 第一版限制每个 token 在每个建库 source 下最多 5 个活动库、每分钟最多成功创建 1 个；source 归档/恢复仅限 admin，归档不删除物理数据库。
- 完整工作区/共享工作区、TTL、快照恢复、自动删库、跨驱动存储硬限额和生产写入暂缓；保留创建者、父 source、环境和生命周期扩展空间。
- 本节更新目标基线，不表示代码已经实现；实现状态和证据以 `TASK.md` 为准，2026-11-11 的条目保留为历史决策。

## 2026-11-11

- 权限模型定为每 agent × source 的三种静态模式：`readonly`、`readwrite`、`approval`；不再使用限时授权、`session` 模式和主动 `revoke`。
- `approval` 按写语句逐条确认：优先 MCP elicitation，客户端不支持时退回 CLI 待批队列（经 SSH 隧道调用管理接口）。
- 服务部署在数据库服务器，只绑 `127.0.0.1`，由本机 SSH 隧道接入；单实例管理多个 source，单用户使用。
- MCP 工具固定为 `list_sources`、`list_tables`、`describe_table`、`explain_query`、`execute_sql`，不随权限变化发送 `tools/list_changed`。
- 连接池由模式决定；SQL 分类器只决定是否需要审批，误判只会失败或多问，不会越权。