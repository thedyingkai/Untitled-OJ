# Action 模型

GUI、TUI 和后端入口使用同一套 action 注册表、form schema、plan schema、result schema 和 error schema。

正式 action 来自 `platform/schemas/orchestrator/actions.yaml`，由 `services/orchestrator/core/src/action.rs`
校验。

Action 按抽象层组织。每个正式层都必须暴露 CRUD 风格的 action：

```text
release
host
service
endpoint
link
route
frontend
migration
permission
redis
storage
config
secret
topology
operation
log
diagnostic
```

每一层都有 `create`、`list`、`get`、`update`、`delete`。像 `validate`、`install`、`apply`、
`health.check`、`query`、`export` 这样的领域动词，只允许作为 CRUD 基础之上的额外 action 出现。

`service-name[*]` 不是正式表、实体或 action 层。它只是对同名运行中 endpoint 的查询。daemon 可以保留
遗留的 `/sets/{id}/expand` 和 `/sets/{id}/apply` HTTP 路由并返回「已废弃/兼容」响应，但 `set.expand`
和 `set.apply` 不是目录 action。

Endpoint 身份始终是：

```text
ip:port:service-name
```

不引入 `instance-id`。

## 执行契约

`ActionRequest` 值可以由 GUI、TUI 或后端 HTTP handler 产生，但 operation 计划必须由 core 产生。入口层
不得自行组装计划、不得改动 operation 状态机、不得绕过 core 执行器。

`services/orchestrator/core/src/dispatcher.rs` 是唯一的 action dispatcher。它读取 action schema、构建
`Operation`、在 action 受支持时写入 store、在可用处调用固定执行路径，并返回 `ActionDispatchResult`。

每个 dispatch 结果都有明确的能力状态：

```text
REAL          执行了真实探测或真实读取，并持久化了可观测结果
STORE_BACKED  写入了 Store、Operation、OperationLog 或视图元数据，但没有外部执行
UNSUPPORTED   当前无法执行所请求的变更，且不得显示为成功
READONLY      计算或读取了数据，但没有改动 core 对象
```

不受支持的目录 action 绝不会走假成功路径。

## 当前能力矩阵

| 层 | 代表性 action | 状态 |
| --- | --- | --- |
| release | `release.create/list/get/update/delete`、`release.validate`、`release.install` | CRUD 加 install 路径；install 是 store-backed |
| service | `service.create/list/get/update/delete`、`service.start/stop/restart/enable/disable`、`service.health.check` | CRUD 已入目录；生命周期变更在有安全 driver 绑定前保持 UNSUPPORTED |
| endpoint | `endpoint.create/list/get/update/delete`、`endpoint.health.check` | store-backed CRUD；健康检查可为真实 |
| link | `link.create/list/get/update/delete`、`link.health.check` | store-backed CRUD；健康检查可为真实 |
| topology | `topology.create/list/get/update/delete`、`topology.validate`、`topology.apply`、`topology.export` | 读/校验/导出受支持；不受支持的变更是显式的 |
| operation | `operation.create/list/get/update/delete`、`operation.confirm`、`operation.apply`、`operation.rollback`、`operation.cancel` | store-backed operation 状态机 |
| log | `log.create/list/get/update/delete`、`log.query` | LogView CRUD 与 operation 日志查询 |
| diagnostic | `diagnostic.create/list/get/update/delete`、`diagnostic.export` | store-backed 报告与导出 |
| host/route/frontend/migration/permission/redis/storage/config/secret | CRUD 基础 | 已入目录，能力显式为 UNSUPPORTED/READONLY，直到实现后端 store |

诊断报告包含 action 能力证据，因此 `STORE_BACKED` 和 `UNSUPPORTED` 路径不会与 `REAL` 执行混淆。

## 后端 API

编排器后端把 HTTP 请求转换为 `ActionRequest`，并调用与 GUI、TUI 相同的 dispatcher。

当前写/读入口为：

```text
POST /actions
POST /endpoints
PATCH /endpoints/{endpoint}
DELETE /endpoints/{endpoint}
POST /endpoints/{endpoint}/health
POST /endpoints/health
POST /links
PATCH /links/{source_endpoint}/{target_endpoint}
DELETE /links/{source_endpoint}/{target_endpoint}
POST /links/{source_endpoint}/{target_endpoint}/health
POST /links/health
POST /operations/plan
POST /operations/{operation_id}/confirm
POST /operations/{operation_id}/apply
POST /operations/{operation_id}/rollback
GET  /operations/{operation_id}/logs
POST /diagnostics
GET  /diagnostics/{report_id}
GET  /diagnostics/{report_id}.json
GET  /diagnostics/{report_id}.md
```

`GET /topology` 从当前 store 状态重建：services、endpoints、links、operations、log views 和 diagnostic
reports。正式的 service-set 持久化不属于 store。

## Driver 边界

Driver 只接受固定 action。任意 shell、任意脚本路径、用户提供的命令字符串和远程 root shell 都在 action
模型之外。

当前固定 driver 为：

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

Driver 能力属于更底层的实现细节。在项目拥有启动、停止和删除服务的安全绑定之前，action console 仍将不受
支持的服务生命周期命令报告为 `UNSUPPORTED`。
