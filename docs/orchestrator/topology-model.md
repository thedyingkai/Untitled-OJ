# Topology 模型

Topology 展示 Orchestrator 当前理解的关系图。节点和边只来自：

```text
Service
Set
Endpoint
Link
Operation
LogView
DiagnosticReport
```

Endpoint 是运行时唯一连接身份，格式为 `IP:Port`。Endpoint 的 IP 部分可以用于按 host 分组展示服务、日志和状态，但不引入额外核心对象。

Topology 至少包含：

- Service 节点。
- Set 对 Service 的推荐组合关系。
- Endpoint 节点及其绑定的 `service_id`。
- Link 边，格式为 `source endpoint -> target endpoint`。
- Operation 对目标对象的影响。
- LogView 和 DiagnosticReport 的关联目标。
- health、reachable、latency 和 degraded 状态。

Topology 可以提供这些视角：

- Set 视角。
- Service 视角。
- Endpoint 视角。
- Link 视角。
- host/IP 分组视角。
- Health 视角。
- Operation 视角。

Topology 的修改必须通过 Operation 计划和确认完成。GUI/TUI 不能绕过 core 直接改写拓扑数据。
