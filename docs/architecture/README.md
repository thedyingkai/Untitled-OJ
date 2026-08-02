# 架构总览

OJOS 采用 service-release-first 架构。`release.yaml` 先声明服务版本、运行方式、API、迁移、权限和资源，
编排器校验发布契约后再生成安装或生命周期 Operation。

编排器持久化和派生的主要对象如下：

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

Endpoint 的运行时身份固定为 `ip:port:service-name`。`service-name[*]` 只是查询同名运行端点的表达式，
不是单独的表或运行时对象。

Service 的 API 声明先落到 `service_api_surfaces`。安装到某个 Endpoint 后生成
`deployed_service_apis`，再结合 Node、Link、可见性和运行状态计算有效路由。Gateway 消费这份路由表，
处理业务流量；它不是编排器控制面。

正式入口为 Web UI、TUI 和 daemon HTTP API。Web UI 通过 daemon 调用 core；TUI 在进程内调用同一套
`OrchestratorActionConsole`。原生 egui GUI 已删除。

本地部署模板和 Set 可用于播种或推荐组合，但不取代 store 中的 Service、Endpoint、Link 和 Operation。

更细的职责选择见[耦合决策](coupling-decisions.md)，数据所有权见[编排器数据库](../orchestrator/database.md)。
