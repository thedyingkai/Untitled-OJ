# 架构文档

本目录只保存已重写并适用于当前结构的正式架构文档。

OJOS Orchestrator 的正式核心对象固定为：

```text
Service
Set
Endpoint
Link
Operation
Topology
LogView
DiagnosticReport
```

架构边界：

- 编排器只负责服务编排、连接、启停、状态查看、日志查看、诊断和操作计划。
- OJ 业务能力由被编排的 Service 提供。
- Endpoint 是运行时唯一连接身份，格式为 `IP:Port`。
- Set 是推荐部署组合，不是运行时对象。
- Gateway 和 Web Shell 都是 Service，不是控制面。
- GUI 和 TUI 是正式入口，并且能力等价。

当前详细规范分布在：

- `docs/spec/`
- `docs/orchestrator/`
- `docs/services/`
- `docs/release/`
