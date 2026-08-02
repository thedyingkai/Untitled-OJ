# 编排器需求

OJOS Orchestrator 是 OJOS 的服务控制面。

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

共享 action 目录按对象层组织，并保留 CRUD 风格名称。`validate`、`install`、`apply`、`health.check`、
`query` 和 `export` 是领域动作。目录中存在某个 action 不代表后端已经支持；调用方必须读取结果里的
`capability_status`。

最小安装单元是 ServiceRelease。它可以声明后端、前端、迁移、权限、路由、Redis、存储、配置、密钥、
依赖和可观测性。发布包不天然包含可执行文件或镜像，运行资产要由交付物另外提供。

Endpoint 身份始终是 `ip:port:service-name`。模型中不存在 `instance-id`。

`service-name[*]` 是对同名运行中 endpoint 的派生查询。本地部署模板可以作为只读辅助材料展示，但它们
不是正式 store 对象，也没有正式 action。

正式入口为 Web UI、TUI 和 daemon HTTP API。三者使用
`services/orchestrator/core` 与 `platform/schemas/orchestrator`；Web UI 通过 daemon REST 调用，TUI
在进程内调用 core。原生 egui GUI 已删除。

所有变更必须经过 plan、必要的确认、execute 和可审计结果。生命周期驱动默认不执行，只有请求显式携带
`execute_service_driver=true` 才能运行固定的本地进程或 Compose 动作。回滚也遵循同一授权要求。

编排器不实现题目、提交、用户、比赛、训练、Clarification、打印、滚榜或站点管理。这些功能属于被管理的
Service。
