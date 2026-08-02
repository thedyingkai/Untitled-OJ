# Action 模型

Web UI、TUI 和 daemon 使用同一套 action 注册表、表单、计划、结果和错误 schema。

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

目录为各层保留 `create`、`list`、`get`、`update`、`delete`，并按需要增加 `validate`、`install`、
`apply`、`health.check`、`query` 和 `export`。目录完整不等于实现完整；调用方要读取
`capability_status`，不能只看 HTTP 2xx。

`service-name[*]` 不是正式表、实体或 action 层。它只是对同名运行中 endpoint 的查询。daemon 可以保留
遗留的 `/sets/{id}/expand` 和 `/sets/{id}/apply` HTTP 路由并返回「已废弃/兼容」响应，但 `set.expand`
和 `set.apply` 不是目录 action。

Endpoint 身份始终是：

```text
ip:port:service-name
```

不引入 `instance-id`。

## 执行契约

`ActionRequest` 可以由 Web UI、TUI 或 daemon HTTP handler 产生，但 Operation 计划必须由 core 产生。入口层
不得自行组装计划、不得改动 operation 状态机、不得绕过 core 执行器。

`services/orchestrator/core/src/dispatcher.rs` 是唯一的 action dispatcher。它读取 action schema、构建
`Operation`、在 action 受支持时写入 store、在可用处调用固定执行路径，并返回 `ActionDispatchResult`。

每个 dispatch 结果都有明确的能力状态：

```text
REAL          执行了真实探测或真实读取，并持久化了可观测结果
RUNTIME_PIPELINE 调用了固定运行时流水线；结果仍可能因驱动未授权或外部依赖失败而 FAILED
STORE_BACKED  写入了 Store、Operation、OperationLog 或视图元数据，但没有外部执行
UNSUPPORTED   当前无法执行所请求的变更，且不得显示为成功
READONLY      计算或读取了数据，但没有改动 core 对象
```

不受支持的目录 action 绝不会走假成功路径。

## 当前能力矩阵

| 层 | 代表性 action | 状态 |
| --- | --- | --- |
| release | `release.*` | create/update/delete 为 STORE_BACKED，list/get/validate 为 READONLY；install/rollback 为 RUNTIME_PIPELINE |
| host | `host.*` | list/get 为 READONLY；start/stop 为 RUNTIME_PIPELINE；其余仍为 UNSUPPORTED |
| service | `service.*` | list/get 为 READONLY；health.check 为 STORE_BACKED；start/stop/restart/delete 为 RUNTIME_PIPELINE；create/update/enable/disable 仍为 UNSUPPORTED |
| endpoint | `endpoint.create/list/get/update/delete`、`endpoint.health.check` | store-backed CRUD；健康检查可为真实 |
| link | `link.create/list/get/update/delete`、`link.enable/disable`、`link.health.check` | store-backed CRUD 与启停；健康检查可为真实 |
| route/frontend/migration/permission/redis/storage/config | 分层 CRUD + 领域动作 | create/update/delete 为 STORE_BACKED，list/get/validate 为 READONLY；apply/publish/sync/render 等外部动作仍为 UNSUPPORTED |
| secret | `secret.*` | list/get 为 READONLY；变更和 distribute 仍为 UNSUPPORTED |
| topology | `topology.*` | list/get/validate/export 为 READONLY；变更与 apply 为 UNSUPPORTED |
| operation | `operation.*` | confirm/cancel 为 STORE_BACKED，list/get 为 READONLY；apply/rollback 继承目标 Operation 的能力，可能是 RUNTIME_PIPELINE；create/update/delete 为 UNSUPPORTED |
| log | `log.*` | create/query 为 STORE_BACKED，list/get 为 READONLY；update/delete 为 UNSUPPORTED |
| diagnostic | `diagnostic.*` | create/export 为 STORE_BACKED，list/get 为 READONLY；update/delete 为 UNSUPPORTED |

诊断报告包含 action 能力证据，因此 `STORE_BACKED` 和 `UNSUPPORTED` 路径不会与 `REAL` 执行混淆。

## 后端 API

daemon 把 HTTP 请求转换为 `ActionRequest`，并调用同一个 dispatcher。

HTTP 入口会从空字段创建请求，再合并调用方提交的内容，不会继承 TUI 预览所用的示例值。必填字段以
`platform/schemas/orchestrator/forms.yaml` 为准；缺少字段时返回 400，不会代填示例 Service、Endpoint、
版本号或确认标记。

`POST /actions` 是不丢字段的通用入口，Web UI 在需要传执行期选项时使用它。常用的对象路由包括：

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
POST /links/{source_endpoint}/{target_endpoint}/enable
POST /links/{source_endpoint}/{target_endpoint}/disable
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

这不是 HTTP 路由全文。Service、Release、Host、Node、商店和 UI 布局还有对象化便捷路由，具体匹配规则以
`services/orchestrator/backend/src/routes.rs` 为准。`POST /operations/{id}/apply` 和
`POST /operations/{id}/rollback` 都接收 JSON 选项；可直接传 `execute_service_driver` 和
`gateway_node_id`，路径里的 Operation ID 始终覆盖 body 中的同名字段。`POST /actions` 仍可用于统一调用。

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

`service.start/stop/restart/delete` 与 `host.start/stop` 已绑定上述 driver，并报告
`RUNTIME_PIPELINE`。生命周期应用和回滚只有在请求携带 `execute_service_driver=true` 时才执行；未授权或驱动
失败会返回 `FAILED`。`service.enable/disable` 仍报告 `UNSUPPORTED`，不能作为新动作派发；但历史记录若已经成功，
显式回滚会在重新取得 driver 授权后执行相反动作，并校验驱动确实成功。

`operation.create` 目前也报告 `UNSUPPORTED`。它只有通用计划外壳，没有“创建目标 Operation”的独立存储语义；
在真实 mutation 接通前，不能把生成一条 changed object 当成执行成功。

Release 动作要分开看。首次 `release.install` 可以只登记元数据，此时不需要 driver 授权，也不会声称固定
runtime 已经启动。若安装会替换正在运行的固定 runtime，请求必须显式传 `execute_service_driver=true`。
`release.rollback` wrapper 每次都要求这项授权；直接对原 `release.install` 调用 `operation.rollback` 时，只有
原安装启用了 driver，回滚才要求再次授权。`release.delete` 不调用 driver；它只删除未被部署引用的 Release
记录，卸载服务应使用 `service.delete`。

跨节点执行还多几道约束。控制面打开 `ORCHESTRATOR_NODE_DISPATCH` 时必须配置
`ORCHESTRATOR_NODE_ENDPOINT`。目标 Node 同时验证专用 bearer 与控制面 token；允许真实 driver 时，
`ORCHESTRATOR_NODE_HOST_IP` 必须与请求主机和 Endpoint host 完全一致。请求已授权 driver、但目标 Node 没打开
`ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER` 时会直接失败，不会改成只写元数据。Node-owned 部署目前不支持远端
升级、回滚或 Service/Host 生命周期，这些动作会明确阻塞。
