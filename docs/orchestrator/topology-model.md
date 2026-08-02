# Topology 模型

Topology 是编排器根据当前 store 重建的服务连接视图。

它由以下对象构建：

```text
Service
Endpoint
Link
Operation
LogView
DiagnosticReport
```

`GET /topology` 每次从 store 读取这些对象。daemon 不使用启动时缓存替代当前数据。内存模式可以从仓库
manifest 播种预览数据，但模板本身不是拓扑对象。

Endpoint 身份是 `ip:port:service-name`。Link 身份是 endpoint 到 endpoint：

```text
source endpoint -> target endpoint
```

Endpoint 健康值为：

```text
healthy
degraded
blocked
unreachable
unknown
```

Link 健康由 source endpoint、target endpoint、target 可达性、协议族、认证模式、scope 以及可选延迟派生。

## Node 与有效 API 路由

Node 树和 Topology 视图相关，但使用独立记录：

- `nodes` 保存 `node_id`、`host_ip`、`parent_node_id`、角色和状态。
- `service_api_surfaces` 保存 release 声明的 API。
- `deployed_service_apis` 保存某个 API 在主机和 Endpoint 上的部署状态。
- 有效路由由以上记录即时计算，不单独持久化。计算会拒绝有环的 Node 树，只选
  `status == "running"` 的部署，再应用 `same-node`、`descendants` 或 `global` 可见性。

Gateway 使用 `/internal/orchestrator/nodes/{node_id}/routes` 拉取路由。运维调用也可使用
`/nodes/{node_id}/routes?include_upstream=true` 查看同一结果。主动推送路由时必须配置
`GATEWAY_NODE_ID`；编排器不会向 Gateway 强推一张无法确定节点范围的空路由表。

相关测试：

```text
daemon_topology_reflects_endpoint_link_mutations
topology_is_rebuilt_from_store_after_actions
topology_reflects_endpoint_link_health
reconcile_tick_snapshot_uses_refreshed_store_state
```
