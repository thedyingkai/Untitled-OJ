# 架构文档

当前架构是 service-release-first（以服务发布为先）。

正式编排器对象为：

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

运行时 endpoint 身份是 `ip:port:service-name`。

`service-name[*]` 是对运行中 endpoint 的派生查询。本地部署模板是只读辅助材料，不是正式运行时对象。

Gateway 与 Gateway 前端是 Service，不是编排器控制平面。GUI 与 TUI 是正式管理入口，必须通过 core 保持
能力等价。
