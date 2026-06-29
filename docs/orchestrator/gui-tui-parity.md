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

两者都能浏览 Service、Set、Endpoint、Link、Topology、Operation、LogView 和 DiagnosticReport。两者都通过 core 工作台生成 plan、confirm、apply、rollback，并查看结果、错误、日志、`created_at` 和 `updated_at`。

两者现在都通过 `OrchestratorActionConsole` 调用 `OrchestratorActionDispatcher`，不直接修改 Store。GUI 的按钮和 TUI 的快捷键会提交同一类 `ActionRequest`，执行后显示 `REAL`、`STORE_BACKED`、`UNSUPPORTED` 或 `READONLY`，并从 Store 重新加载视图。

当前入口默认可以从仓库文件生成本地视图；当存在 `ORCHESTRATOR_DATABASE_URL` 时，`load_orchestrator_view` 会优先尝试 `PgOrchestratorStore`，读取真实 Orchestrator Store 状态。数据库不可用时会回落本地视图并显示 warning。

Operation 工作台同样由 core 统一选择 Store。未设置 `ORCHESTRATOR_DATABASE_URL` 时，plan、confirm、apply、rollback 使用 `MemoryOrchestratorStore` 进行本地演示；设置该变量时，工作台 context 会先从 `PgOrchestratorStore` 读取当前 Service、Set、Endpoint、Link、Topology，再把 plan/update 写成 `PLANNED` Operation，confirm 写成 `AWAITING_CONFIRMATION`，apply/rollback 由 `OperationExecutor` 写入 operation 状态、step log、result 和 rollback 记录。

GUI/TUI 不包含 OJ 业务后台功能，不安装 Service，不自行管理 Endpoint/Link，不修改 Topology，不充当 Gateway 或 Web Shell 控制面。

GUI/TUI 操作证据：

```text
gui_exposes_dispatcher_backed_actions
tui_exposes_dispatcher_backed_actions
gui_fonts_force_cjk_fallback_for_all_text_styles
gui_source_keeps_utf8_chinese_text_without_mojibake
tui_source_keeps_utf8_chinese_text_without_mojibake
```
