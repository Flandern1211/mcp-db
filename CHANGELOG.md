# 变更记录

## 2026-11-11

- 权限模型定为每 agent × source 的三种静态模式：`readonly`、`readwrite`、`approval`；不再使用限时授权、`session` 模式和主动 `revoke`。
- `approval` 按写语句逐条确认：优先 MCP elicitation，客户端不支持时退回 CLI 待批队列（经 SSH 隧道调用管理接口）。
- 服务部署在数据库服务器，只绑 `127.0.0.1`，由本机 SSH 隧道接入；单实例管理多个 source，单用户使用。
- MCP 工具固定为 `list_sources`、`list_tables`、`describe_table`、`explain_query`、`execute_sql`，不随权限变化发送 `tools/list_changed`。
- 连接池由模式决定；SQL 分类器只决定是否需要审批，误判只会失败或多问，不会越权。