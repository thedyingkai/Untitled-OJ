# GUI / TUI 等价性

`manager/gui` 和 `manager/tui` 是等价的管理入口。它们只在交互形态上不同，编排能力上不能有差异。

两者都使用：

```text
services/orchestrator/core
platform/schemas/orchestrator/actions.yaml
platform/schemas/orchestrator/forms.yaml
platform/schemas/orchestrator/plans.yaml
platform/schemas/orchestrator/results.yaml
platform/schemas/orchestrator/errors.yaml
```

两者浏览相同的 store-backed 对象：service releases、services、endpoints、links、topology、operations、
log views 和 diagnostic reports。Template 页面只是本地部署模板的只读视图；`service-name[*]` 是派生的
endpoint 查询，不是正式 action 层。

两者都调用 `OrchestratorActionConsole`，后者委托给 `OrchestratorActionDispatcher`。GUI 按钮和 TUI 快捷键
提交相同形态的 `ActionRequest`。结果显示 `REAL`、`STORE_BACKED`、`UNSUPPORTED` 或 `READONLY`，然后重新
加载 store-backed 视图。

## 暴露的 action

GUI 暴露：

```text
Endpoint 页面：create、update、delete、health check
Link 页面：create、update、delete、health check
Operation 页面：confirm、apply、rollback、logs
Diagnostics 页面：create report、export JSON、export Markdown
Action workbench：共享 CRUD 层注册表中的每一个目录 action
```

TUI 暴露等价的快捷键：

```text
Endpoint Actions: e create / E update / x delete / h health check
Link Actions: l create / L update / X delete / H health check
Operation Actions: c confirm / a apply / u rollback / o logs
Diagnostics: d create / D export markdown
```

两个入口都没有正式的部署模板 action。

## Store 选择

在没有 `ORCHESTRATOR_DATABASE_URL` 时，入口从仓库 manifest 构建本地视图，并使用 `MemoryOrchestratorStore`
做演示 workbench 操作。

在设置了 `ORCHESTRATOR_DATABASE_URL` 时，`load_orchestrator_view`、`OperationWorkbenchContext` 和
`OrchestratorActionConsole` 使用 `PgOrchestratorStore`。如果数据库不可用，入口返回错误，而不是回退到仓库
fixture。

workbench 同样由 core 选择。它通过与后端 action 相同的 store 抽象来计划、确认、应用、回滚、写 operation
日志并刷新视图状态。

GUI/TUI 不包含 OJ 业务后端功能、不充当 Gateway、也不绕过 core action dispatcher。

## 证据

```text
gui_exposes_dispatcher_backed_actions
tui_exposes_dispatcher_backed_actions
gui_endpoint_actions_are_directly_available
gui_link_actions_are_directly_available
gui_diagnostics_export_is_directly_available
gui_action_feedback_shows_capability_status
tui_endpoint_action_menu_exists
tui_link_action_menu_exists
tui_diagnostics_action_exists
tui_action_feedback_shows_capability_status
gui_fonts_force_required_cjk_font_for_all_text_styles
orchestrator_code_forbids_lossy_text_decoding
gui_source_keeps_utf8_chinese_text_without_mojibake
tui_source_keeps_utf8_chinese_text_without_mojibake
```
