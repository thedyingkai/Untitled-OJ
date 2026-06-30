# GUI / TUI Parity

`manager/gui` and `manager/tui` are equivalent management entry points. They differ only in interaction style, not in orchestration capability.

Both use:

```text
services/orchestrator/core
platform/schemas/orchestrator/actions.yaml
platform/schemas/orchestrator/forms.yaml
platform/schemas/orchestrator/plans.yaml
platform/schemas/orchestrator/results.yaml
platform/schemas/orchestrator/errors.yaml
```

Both browse the same store-backed objects: service releases, services, endpoints, links, topology, operations, log views, and diagnostic reports. The Template page is only a read-only view of local deployment templates; `service-name[*]` is a derived endpoint query, not a formal action layer.

Both call `OrchestratorActionConsole`, which delegates to `OrchestratorActionDispatcher`. GUI buttons and TUI shortcuts submit the same `ActionRequest` shape. Results show `REAL`, `STORE_BACKED`, `UNSUPPORTED`, or `READONLY`, then reload the store-backed view.

## Exposed Actions

GUI exposes:

```text
Endpoint page: create, update, delete, health check
Link page: create, update, delete, health check
Operation page: confirm, apply, rollback, logs
Diagnostics page: create report, export JSON, export Markdown
Action workbench: every catalog action from the shared CRUD-layer registry
```

TUI exposes equivalent shortcuts:

```text
Endpoint Actions: e create / E update / x delete / h health check
Link Actions: l create / L update / X delete / H health check
Operation Actions: c confirm / a apply / u rollback / o logs
Diagnostics: d create / D export markdown
```

There are no formal deployment-template actions in either entry point.

## Store Selection

Without `ORCHESTRATOR_DATABASE_URL`, the entries build a local view from repository manifests and use `MemoryOrchestratorStore` for demo workbench operations.

With `ORCHESTRATOR_DATABASE_URL`, `load_orchestrator_view`, `OperationWorkbenchContext`, and `OrchestratorActionConsole` use `PgOrchestratorStore`. If the database is unavailable, the entry returns an error instead of falling back to repository fixtures.

The workbench is also selected by core. It plans, confirms, applies, rolls back, writes operation logs, and refreshes view state through the same store abstraction used by backend actions.

GUI/TUI do not contain OJ business backend features, do not act as Gateway, and do not bypass the core action dispatcher.

## Evidence

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
