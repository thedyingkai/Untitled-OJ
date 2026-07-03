# Topology 模型

Topology 是编排器对当前已知服务与运行时连通性的视图。

它由以下对象构建：

```text
Service
Endpoint
Link
Operation
LogView
DiagnosticReport
```

在未配置持久化 store 时，本地部署模板可以播种预览用的 endpoint 和 link，但模板不是正式拓扑对象。

Endpoint 身份是 `ip:port:service-name`。Link 身份是 endpoint 到 endpoint：

```text
source endpoint -> target endpoint
```

`GET /topology` 从当前 store 重建。它必须反映通过 action dispatch 写入的 Endpoint 与 Link 变更。daemon
不得用陈旧的启动上下文替代 store-backed 的拓扑。

Endpoint 健康值为：

```text
healthy
degraded
blocked
unreachable
unknown
```

Link 健康由 source endpoint、target endpoint、target 可达性、协议族、认证模式、scope 以及可选延迟派生。

相关证据：

```text
daemon_topology_reflects_endpoint_link_mutations
topology_is_rebuilt_from_store_after_actions
topology_reflects_endpoint_link_health
reconcile_tick_snapshot_uses_refreshed_store_state
```
