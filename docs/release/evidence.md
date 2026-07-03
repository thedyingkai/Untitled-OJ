# 可核对证据

本文件记录当前 OJOS Orchestrator 的证据集。

正式实现入口：

```text
services/orchestrator/core/
services/orchestrator/backend/
manager/gui/
manager/tui/
platform/schemas/orchestrator/
```

历史文档已从仓库移除，其架构结论已并入 [项目完成度总结](../completeness-summary.md)。

## 已实现证据

- Store 抽象：`services/orchestrator/core/src/store.rs`
- 持久化 store：`services/orchestrator/core/src/database.rs` 中的 `PgOrchestratorStore`，使用 `ORCHESTRATOR_DATABASE_URL`
- 被忽略的 PostgreSQL 集成测试：`services/orchestrator/core/tests/pg_store_integration.rs`
- 持久化入口诚实性：`orchestrator_entrypoints_require_reachable_persistent_store_when_database_url_is_set`
- Operation 状态机与执行器：`services/orchestrator/core/src/model.rs` 与 `services/orchestrator/core/src/store.rs`
- 固定 driver 安全性：`docker_compose_driver_rejects_unknown_action`、`external_endpoint_driver_does_not_start_services`、`unsupported_driver_action_writes_operation_log`
- Endpoint 身份：`endpoint_requires_ip_port_service_name`、`topology_uses_endpoint_identity_without_machine_or_installation`
- Endpoint 与 link 的 CRUD/健康：`endpoint_create_update_delete_and_health_write_store`、`link_create_update_delete_and_health_write_store`、`endpoint_http_health_updates_store`、`endpoint_tcp_health_updates_store`、`endpoint_unreachable_is_recorded`
- Action 目录与 CRUD 层覆盖：`action_registry_contains_required_actions_and_no_forbidden_actions`、`core_action_catalog_covers_registry_and_core_objects`、`form_registry_covers_every_action`
- Service-set 降级：`set_expand_apply_are_not_formal_console_actions`，以及证明 `service_sets` 不被持久化的数据库测试
- GUI/TUI dispatcher 入口：`gui_exposes_dispatcher_backed_actions`、`tui_exposes_dispatcher_backed_actions`
- GUI/TUI 直接 action：`gui_endpoint_actions_are_directly_available`、`gui_link_actions_are_directly_available`、`gui_diagnostics_export_is_directly_available`、`tui_endpoint_action_menu_exists`、`tui_link_action_menu_exists`、`tui_diagnostics_action_exists`
- Daemon 路由：`daemon_endpoint_routes_use_core_dispatcher`、`daemon_endpoint_health_route_dispatches_action`、`daemon_link_health_route_dispatches_action`、`daemon_set_expand_route_is_gone`、`daemon_set_apply_route_is_gone`、`daemon_operation_routes_expose_operation_state_and_logs`、`daemon_operation_rollback_route_dispatches_action`、`daemon_diagnostic_route_uses_core_diagnostic_report`、`daemon_diagnostics_export_routes_work`
- 拓扑刷新：`daemon_topology_reflects_endpoint_link_mutations`、`topology_is_rebuilt_from_store_after_actions`、`topology_reflects_endpoint_link_health`
- UTF-8 源码安全：`gui_source_keeps_utf8_chinese_text_without_mojibake`、`tui_source_keeps_utf8_chinese_text_without_mojibake`、`daemon_decodes_http_requests_as_strict_utf8`

## 当前限制

- 服务生命周期 action 已入目录，但在存在安全执行绑定之前，通过 action console 仍为 `UNSUPPORTED`。
- LocalProcessDriver 不是生产级监督进程。
- DockerComposeDriver 只接受固定命令形态。
- 远程部署 agent 与跨主机滚动发布不是完整的生产能力。

## 最近本地验证

```powershell
cargo test -p orchestrator-core action -- --nocapture
cargo test -p orchestrator-core endpoint -- --nocapture
cargo test -p orchestrator-core database -- --nocapture
cargo test -p ojos-orchestrator-daemon -- --nocapture
cargo test -p ojos-orchestrator-gui -- --nocapture
cargo test -p ojos-orchestrator-tui -- --nocapture
git diff --check
```
