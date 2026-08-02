# 可核对证据

本页给出实现入口、测试名和证据边界。远端运行结果另见 [生产就绪证据](../production-readiness.md)；两类证据不要混用。

## 实现入口

```text
services/orchestrator/core/
services/orchestrator/backend/
manager/web/
manager/tui/
platform/schemas/orchestrator/
```

原生 `manager/gui` 已删除。浏览器通过 daemon 的 REST API 使用同一套 action dispatcher，TUI 仍直接复用 core。

## 代码与测试索引

- Store 与持久化：`services/orchestrator/core/src/store.rs`、`services/orchestrator/core/src/database.rs`，数据库变量为 `ORCHESTRATOR_DATABASE_URL`。PostgreSQL 集成测试在 `services/orchestrator/core/tests/pg_store_integration.rs`。
- Operation 状态机：`services/orchestrator/core/src/model.rs`；计划、确认、执行和回滚由 `OperationExecutor` 落库并写日志。
- Service/Release 契约：`services/orchestrator/core/src/service.rs`。多版本选择由
  `release_install_planner_selects_service_manifest_version` 覆盖；自动回滚目标的时间顺序由
  `release_rollback_target_uses_operation_timestamps_not_store_order` 覆盖。
- Release 包校验：`release_package_loader_enforces_required_checksum_for_every_entry_point`、`release_install_fails_on_release_package_checksum_mismatch`。
- Driver 边界：`docker_compose_driver_rejects_unknown_action`、`docker_compose_driver_runs_only_when_explicitly_enabled`、`external_endpoint_driver_does_not_start_services`。
- 本地进程与 Release 删除边界：`concurrent_local_process_start_reserves_pid_file_atomically`、
  `release_delete_historical_version_keeps_current_deployment_intact`、
  `release_delete_rejects_deployed_version_without_touching_runtime_or_registry`。
- Release 升级与回滚：`running_fixed_runtime_upgrade_requires_driver_authorization`、
  `authorized_release_upgrade_and_rollback_restore_running_runtime`、
  `authorized_release_upgrade_rollback_keeps_previous_stopped_runtime_stopped`、
  `release_rollback_wrapper_cannot_claim_a_second_rollback`。
- Endpoint 身份：`endpoint_requires_ip_port_service_name`、`topology_uses_endpoint_identity_without_machine_or_installation`。
- Endpoint CRUD 与健康：`endpoint_create_update_delete_and_health_write_store`、`endpoint_http_health_updates_store`、`endpoint_tcp_health_updates_store`、`endpoint_unreachable_is_recorded`。
- Link CRUD、启停与回滚：`link_create_update_delete_and_health_write_store`、`link_disable_and_enable_round_trip_through_operation_chain`、`idempotent_link_toggle_rollback_restores_previous_enabled_state`、`link_update_without_enabled_preserves_disabled_state`。
- 已停用 Link 的边界：`disabled_link_is_excluded_from_diagnostic_unhealthy_links`、`reconcile_tick_skips_disabled_link_health_probe`。
- Service 与主机生命周期：`service_start_plan_carries_release_manifest_and_endpoint`、`host_stop_and_start_round_trip_updates_status_and_routes`。执行前会把 `HostService` 和 `DeployedServiceApi` 快照写进 Operation，显式回滚按旧状态恢复。
- Action/schema 覆盖：`action_registry_contains_required_actions_and_no_forbidden_actions`、`core_action_catalog_covers_registry_and_core_objects`、`form_registry_covers_every_action`。
- 部署模板边界：`set_expand_apply_are_not_formal_console_actions`，且数据库 schema 不持久化 `service_sets`。
- Daemon 路由：`daemon_endpoint_routes_use_core_dispatcher`、`daemon_operation_routes_expose_operation_state_and_logs`、`daemon_operation_rollback_route_dispatches_action`、`daemon_diagnostics_export_routes_work`。
- 控制面鉴权：`internal_token_check_leaves_only_health_open`、`internal_token_check_guards_mutations_and_internal_reads`。
- Service API 鉴权边界：`service_release_api_surface_validation_rules_are_enforced`、
  `TestServiceProxyInternalAPIWithoutTailKeepsServiceCallerCredential`、
  `TestServiceProxyPermissionAPINonAuthProviderDropsServiceCredential`、
  `TestServiceProxyRejectsPublicPermissionForServiceAuth`。
- HTTP 解析：`daemon_decodes_http_requests_as_strict_utf8`、`oversized_content_length_is_rejected_without_overflow`。
- 商店安装状态：`repository_catalog_is_not_reported_as_installed`，目录条目不会再被误报为已安装部署。
- 节点令牌：`daemon_node_install_route_accepts_node_and_control_plane_tokens_together` 覆盖节点 bearer 与控制面 token 并存。
- TUI：`tui_loads_shared_orchestrator_view_from_core`、
  `tui_release_shortcuts_do_not_reuse_preview_paths_or_versions`，以及 TUI 内的 Endpoint、Link、诊断与商店菜单测试。

## 尚不能从这些测试推出的结论

- `LocalProcessDriver` 只保存 PID 文件，没有生产级监督、存活恢复和 PID 复用保护。
- `DockerComposeDriver` 只接受固定动作，且依赖目标环境中的 Compose 文件与 Docker。
- 当前打包产物主要携带 manifest、schema、模板和 Web 静态文件。它不会自动提供业务服务源码、binary 或 image。
- 节点侧 install rollback 仍未实现，跨主机滚动发布也没有完整生产证据。
- PostgreSQL store 没有连接池，连接使用 `NoTls`。

## 本地复核命令

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets

Get-ChildItem -Recurse -Filter go.mod services,platform |
  ForEach-Object { Push-Location $_.DirectoryName; go test ./... -count=1; go vet ./...; Pop-Location }

Push-Location manager/web
npm ci
npm audit
npm run typecheck
npm run build
Pop-Location

git diff --check
```

完成修改后要在干净 checkout 和候选 commit 上重跑，不能只引用开发过程中的一次本地成功。
