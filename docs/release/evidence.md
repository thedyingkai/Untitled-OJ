# 可核对证据

本文件记录当前 OJOS Orchestrator 的可核对证据。

正式实现入口：

```text
orchestrator/core/
orchestrator/gui/
orchestrator/tui/
orchestrator/schemas/
```

正式文档入口：

```text
docs/spec/
docs/orchestrator/
docs/architecture/
docs/services/
docs/release/
```

`docs-temp/` 是历史文档隔离区，不作为当前正式架构依据。

## 当前已实现证据

- Store trait：`orchestrator/core/src/store.rs`
- Memory Store：`MemoryOrchestratorStore`，用于测试、本地开发和无数据库演示
- Pg Store：`PgOrchestratorStore`，位于 `orchestrator/core/src/database.rs`，使用 `ORCHESTRATOR_DATABASE_URL`
- Pg Store ignored integration：`orchestrator/core/tests/pg_store_integration.rs`
- Operation 状态机：`orchestrator/core/src/model.rs`
- Operation executor：`orchestrator/core/src/store.rs`，apply/rollback 都通过 Store 写入 OperationLog；Service 生命周期会记录固定 `DriverResult`；rollback 会记录历史日志读取和 `rollback_plan.steps`
- Store-backed Operation 生命周期直接证据：`operation_plan_is_persisted_in_store`、`operation_confirm_updates_store`、`operation_apply_writes_status_and_logs`、`operation_apply_failure_writes_error_message`、`operation_rollback_updates_store`、`operation_logs_can_be_reopened`、`workbench_uses_store_backed_operation_lifecycle`、`operation_lock_prevents_parallel_apply`
- Executor drivers：`orchestrator/core/src/executor.rs`，覆盖测试为 `executor_rejects_arbitrary_shell`、`docker_compose_driver_builds_allowed_commands`、`docker_compose_driver_rejects_unknown_action`、`local_process_driver_reports_unsupported_safely`、`external_endpoint_driver_does_not_start_services`、`unsupported_driver_action_writes_operation_log`
- Service lifecycle 最小执行：默认路径阻止 Docker Compose plan-only 假成功；显式开启固定 driver 执行后会运行固定命令并把失败写入 Operation。覆盖测试为 `service_start_uses_driver`、`service_stop_uses_driver`、`service_restart_uses_driver`、`service_logs_view_uses_log_source`、`service_lifecycle_failure_is_recorded`、`service_lifecycle_unsupported_is_not_success`
- Endpoint/Link health：`orchestrator/core/src/health.rs`
- Endpoint health 真实探测：HTTP/HTTPS 请求 `health_path`，TCP/Postgres/Redis 使用 TCP connect；覆盖测试为 `endpoint_http_health_updates_store`、`endpoint_tcp_health_updates_store`、`endpoint_unreachable_is_recorded`、`tcp_probe_checks_http_health_path_status`、`tcp_probe_marks_http_non_success_status_as_degraded`
- Link health 写回与 Topology 展示：覆盖测试为 `link_health_requires_existing_endpoints`、`link_health_uses_target_reachability`、`operation_executor_persists_link_health_from_target_probe`、`topology_reflects_endpoint_link_health`
- LogView 查询与 DiagnosticReport 导出：`orchestrator/core/src/observability.rs`
- `operation.logs.view` 与 `diagnostics.export` 执行证据：`orchestrator/core/src/store.rs` 会创建 operation-scoped LogView、写入导出元数据 OperationLog；覆盖测试为 `operation_executor_materializes_operation_log_view_and_diagnostic_export`
- Reconcile tick / loop：`orchestrator/core/src/reconciler.rs`，`run_reconcile_tick` 执行单次刷新，`run_reconcile_loop` 提供可停止的 bounded loop 原语；覆盖测试为 `reconcile_loop_runs_bounded_ticks_and_can_stop`
- GUI/TUI 共享视图：`orchestrator/core/src/view.rs`
- GUI/TUI 共享工作台：`orchestrator/core/src/workbench.rs`，未设置 `ORCHESTRATOR_DATABASE_URL` 时使用 Memory store，设置后通过 `load_operation_workbench_context_from_store` 读取 Store 中的 Service、Set、Endpoint、Link、Topology，并让 plan/confirm/apply/rollback 走 `PgOrchestratorStore`；覆盖测试为 `operation_workbench_context_can_load_from_store_state`
- GUI/TUI Action Console：`orchestrator/core/src/dispatcher.rs`，GUI/TUI 只提交 `ActionRequest`，dispatcher 返回 `REAL`、`STORE_BACKED`、`UNSUPPORTED` 或 `READONLY`；覆盖测试为 `action_dispatcher_routes_schema_actions`、`endpoint_register_update_delete_and_health_write_store`、`link_create_update_delete_and_health_write_store`、`set_expand_apply_and_diagnostic_report_are_console_actions`、`operation_plan_confirm_apply_rollback_and_logs_are_visible`
- GUI/TUI 操作入口：`gui_exposes_dispatcher_backed_actions`、`tui_exposes_dispatcher_backed_actions`
- GUI 字体证据：`gui_fonts_force_cjk_fallback_for_all_text_styles`，GUI 启动时强制加载中文字体 fallback，避免 CJK 字符显示为方块
- GUI/TUI Operation/LogView 观测：`orchestrator/core/src/view.rs` 从 Store 读取 Operation 与 OperationLog，GUI/TUI 展示 `operation_id`、状态、结果、错误、日志数量、日志消息、`created_at` 和 `updated_at`
- Endpoint / Link 表单证据：`orchestrator/schemas/forms.yaml` 覆盖 Endpoint 的 `config` 和 Link 的 `scope`、`config_ref`、`secret_ref`、`policy`；`orchestrator/core/src/planner.rs` 会解析 JSON 字段并写入 Store，覆盖测试为 `endpoint_register_update_delete_and_health_write_store`、`link_create_update_delete_and_health_write_store`
- DiagnosticReport 能力矩阵证据：`build_diagnostic_report` 写入 `action_matrix` 和 `unsupported_capabilities`，覆盖测试为 `diagnostic_report_json_exports_observable_summary`、`diagnostic_report_builds_from_store_and_exports_json_and_markdown`
- Orchestrator migration：`deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql`

## 当前限制

- LocalProcessDriver 尚未接入安全 supervisor，因此生命周期动作返回 Unsupported。
- DockerComposeDriver 默认只返回固定命令计划并阻止假成功；显式执行模式会运行固定 `docker compose` 参数数组，实际成功仍取决于本机 Docker/Compose 环境。
- 远程部署 agent、跨主机发布系统和完整生产 daemon 尚未实现。

## 最近本地验证

```powershell
cargo fmt --check
cargo check -p orchestrator-core -p ojos-orchestrator-gui -p ojos-orchestrator-tui
cargo test -p orchestrator-core
cargo test -p ojos-orchestrator-gui
cargo test -p ojos-orchestrator-tui
```

结果：通过。Go 与 Frontend 本轮未触及，未重跑。
