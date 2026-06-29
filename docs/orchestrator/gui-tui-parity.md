# GUI / TUI 等价性

`orchestrator/gui` 和 `orchestrator/tui` 是两个等价入口。差别只能是交互形态，不是能力差异。

两者共同使用：

```text
orchestrator/core
orchestrator/schemas/actions.yaml
orchestrator/schemas/forms.yaml
orchestrator/schemas/plans.yaml
orchestrator/schemas/results.yaml
orchestrator/schemas/errors.yaml
```

两者都能浏览 Service、Set、Endpoint、Link、Topology、Operation、LogView 和 DiagnosticReport。两者都通过 core 工作台生成 plan、confirm、apply、rollback，并查看结果、错误与日志。

当前入口默认可以从仓库文件生成本地视图；当存在 `ORCHESTRATOR_DATABASE_URL` 时，`load_orchestrator_view` 会优先尝试 `PgOrchestratorStore`，读取真实 Orchestrator Store 状态。数据库不可用时会回落本地视图并显示 warning。

GUI/TUI 不包含 OJ 业务后台功能，不安装 Service，不自行管理 Endpoint/Link，不修改 Topology，不充当 Gateway 或 Web Shell 控制面。
