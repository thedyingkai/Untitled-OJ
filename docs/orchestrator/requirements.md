# 编排器需求

产品名称为 OJOS Orchestrator（OJOS 编排器）。

编排器管理以下正式层：

```text
ServiceRelease
Host
Service
Endpoint
Link
Route
FrontendEntry
Migration
Permission
RedisResource
StorageResource
Config
Secret
Topology
Operation
LogView
DiagnosticReport
```

每个正式 action 层都暴露 CRUD 风格的 action。像 `validate`、`install`、`apply`、`health.check`、
`query`、`export` 这样的额外动词是特定层的补充，不能替代对 CRUD 的完整覆盖。

最小安装单元是 service release（服务发布）。一个 service release 可以携带后端、前端、迁移、权限、
路由、Redis、存储、配置、密钥、依赖和可观测性声明。

Endpoint 身份始终是 `ip:port:service-name`。模型中不存在 `instance-id`。

`service-name[*]` 是对同名运行中 endpoint 的派生查询。本地部署模板可以作为只读辅助材料展示，但它们
不是正式 store 对象，也没有正式 action。

正式入口为 Orchestrator GUI、Orchestrator TUI 和 Orchestrator daemon。三者必须调用同一套
`services/orchestrator/core` 与 `platform/schemas/orchestrator` 契约，差别只能是交互形态或传输形态，
不能是能力差异。

编排器不实现 OJ 业务功能，如题目、提交、用户、比赛、训练、Clarification、打印、滚榜或站点管理。这些
功能属于被管理的具体 Service。
