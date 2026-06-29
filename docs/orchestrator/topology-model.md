# Topology 模型

Topology 展示 Orchestrator 当前理解的关系图。节点和关系只来自：

```text
Service
Set
Endpoint
Link
Operation
LogView
DiagnosticReport
```

Endpoint 是运行时服务实例身份，格式固定为 `IP:Port`，并直接绑定 `service_id`。Link 固定为：

```text
source endpoint -> target endpoint
```

Topology 不通过额外的主机、设备或安装实例模型包装运行实例。

Topology snapshot 存入 `topology_snapshots`，字段为 `snapshot_id`、`topology`、`created_at`。`topology` JSON 中包含 Service、Set、Endpoint、Link、Operation、LogView、DiagnosticReport 的关系。

Endpoint health 可显示：

```text
healthy
degraded
blocked
unreachable
unknown
```

Link health 同样使用这些状态，并可附带 `latency_ms`。Topology 修改必须通过 Operation plan、confirm 和 apply 完成。

GUI/TUI 中的 Endpoint/Link 操作通过 Action Dispatcher 写 Store。`endpoint.register/update/delete` 与 `link.create/update/delete` 会产生 Operation 和 OperationLog；`endpoint.health.check` 与 `link.health.check` 会执行真实探测或基于已登记 Endpoint 状态检查，并把 health 写回 Store。HTTP/HTTPS Endpoint 会请求 `health_path`，2xx/3xx 记为 `healthy`，其他 HTTP 状态记为 `degraded`，连接、TLS、解析或超时失败记为 `unreachable`。TCP、Postgres 和 Redis Endpoint 先做 TCP connect 检查。Link health 会检查 source endpoint、target endpoint、target reachability、protocol family、auth_mode 和 scope。Topology 视图只反映 Store 中已经登记的 Endpoint、Link、Operation、LogView 和 DiagnosticReport。

DiagnosticReport 从 Topology 和 Store 中的 Operation log 生成可导出摘要。Topology 中只展示已登记的 LogView 和 DiagnosticReport，不提供任意路径读取或远程 shell。
