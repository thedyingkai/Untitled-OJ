# Verifiable Evidence

This file records the current OJOS Orchestrator evidence set.

Formal implementation entry points:

```text
services/orchestrator/core/
services/orchestrator/backend/
manager/gui/
manager/tui/
platform/schemas/orchestrator/
```

`docs-temp/` is historical material and is not current architecture evidence.

## Implemented Evidence

- Store abstraction: `services/orchestrator/core/src/store.rs`
- Persistent store: `PgOrchestratorStore` in `services/orchestrator/core/src/database.rs`, using `ORCHESTRATOR_DATABASE_URL`
- Ignored PostgreSQL integration: `services/orchestrator/core/tests/pg_store_integration.rs`
- Persistent entrypoint honesty: `orchestrator_entrypoints_require_reachable_persistent_store_when_database_url_is_set`
- Operation state machine and executor: `services/orchestrator/core/src/model.rs` and `services/orchestrator/core/src/store.rs`
- Fixed driver safety: `docker_compose_driver_rejects_unknown_action`, `external_endpoint_driver_does_not_start_services`, `unsupported_driver_action_writes_operation_log`
- Endpoint identity: `endpoint_requires_ip_port_service_name`, `topology_uses_endpoint_identity_without_machine_or_installation`
- Endpoint and link CRUD/health: `endpoint_create_update_delete_and_health_write_store`, `link_create_update_delete_and_health_write_store`, `endpoint_http_health_updates_store`, `endpoint_tcp_health_updates_store`, `endpoint_unreachable_is_recorded`
- Action catalog and CRUD layer coverage: `action_registry_contains_required_actions_and_no_forbidden_actions`, `core_action_catalog_covers_registry_and_core_objects`, `form_registry_covers_every_action`
- Service-set demotion: `set_expand_apply_are_not_formal_console_actions`, plus database tests proving `service_sets` is not persisted
- GUI/TUI dispatcher entry: `gui_exposes_dispatcher_backed_actions`, `tui_exposes_dispatcher_backed_actions`
- GUI/TUI direct actions: `gui_endpoint_actions_are_directly_available`, `gui_link_actions_are_directly_available`, `gui_diagnostics_export_is_directly_available`, `tui_endpoint_action_menu_exists`, `tui_link_action_menu_exists`, `tui_diagnostics_action_exists`
- Daemon routes: `daemon_endpoint_routes_use_core_dispatcher`, `daemon_endpoint_health_route_dispatches_action`, `daemon_link_health_route_dispatches_action`, `daemon_set_expand_route_is_gone`, `daemon_set_apply_route_is_gone`, `daemon_operation_routes_expose_operation_state_and_logs`, `daemon_operation_rollback_route_dispatches_action`, `daemon_diagnostic_route_uses_core_diagnostic_report`, `daemon_diagnostics_export_routes_work`
- Topology refresh: `daemon_topology_reflects_endpoint_link_mutations`, `topology_is_rebuilt_from_store_after_actions`, `topology_reflects_endpoint_link_health`
- UTF-8 source safety: `gui_source_keeps_utf8_chinese_text_without_mojibake`, `tui_source_keeps_utf8_chinese_text_without_mojibake`, `daemon_decodes_http_requests_as_strict_utf8`

## Current Limits

- Service lifecycle actions are cataloged but remain `UNSUPPORTED` through the action console until a safe execution binding exists.
- LocalProcessDriver is not a production supervisor.
- DockerComposeDriver only accepts fixed command shapes.
- Remote deployment agent and cross-host rollout are not complete production surfaces.

## Recent Local Verification

```powershell
cargo test -p orchestrator-core action -- --nocapture
cargo test -p orchestrator-core endpoint -- --nocapture
cargo test -p orchestrator-core database -- --nocapture
cargo test -p ojos-orchestrator-daemon -- --nocapture
cargo test -p ojos-orchestrator-gui -- --nocapture
cargo test -p ojos-orchestrator-tui -- --nocapture
git diff --check
```
