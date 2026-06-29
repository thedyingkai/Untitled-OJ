# Orchestrator 需求

正式产品名是 OJOS Orchestrator，中文名是 OJOS 编排器。

Orchestrator 只负责这些核心对象：

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

Orchestrator 负责：

- Service 导入、校验、安装计划、启停和删除计划。
- Set 导入、校验、展开、比较和应用计划。
- Endpoint 注册、更新、删除和健康检查。
- Link 创建、更新、删除、健康检查和延迟观测。
- Topology 加载、校验、应用计划和导出。
- Operation 计划、确认、执行、取消、回滚和日志查看。
- 日志视图和诊断报告。

Orchestrator 不负责题库、提交、比赛、用户、权限业务后台、公告、训练、Clarification、打印、滚榜、站点前台或站点后台。这些能力必须由被编排的 Service 自己实现。

正式入口包括 Orchestrator GUI、Orchestrator TUI 和 Orchestrator daemon。GUI/TUI 面向人工交互；daemon 提供最小 HTTP Orchestrator API。三者必须调用同一套 `orchestrator/core` 与 `orchestrator/schemas`，差别只能是交互形态或传输形态，不是能力差异。

当前实现聚焦编排器核心：`orchestrator/core` 提供契约解析、Set 展开、Endpoint / Link 模型、Topology 构建、Operation 状态转换、Store-backed 执行模型、数据库结构检查和视图模型。`orchestrator/gui`、`orchestrator/tui` 与 `orchestrator/daemon` 是正式入口，读取同一套 schema，并通过 core 获取能力。
