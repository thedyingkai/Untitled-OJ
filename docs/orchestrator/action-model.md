# Action 模型

GUI 和 TUI 使用同一套 Action Registry、Form Schema、Plan Schema、Result Schema 和 Error Schema。正式 action 来自 `orchestrator/schemas/actions.yaml`，并由 `orchestrator/core/src/action.rs` 校验。

Action 只能围绕：

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

当前正式 action 包括 service、set、endpoint、link、topology、operation 和 diagnostics 前缀下的固定动作。禁止引入 OJ 业务后台动作、Gateway 控制动作、Web Shell 管理动作、脚本动作、任意包动作或独立来源/产物对象动作。

ActionRequest 由 GUI/TUI 表单产生，但 Operation plan 必须由 core 生成。入口层不能自己拼装 plan、修改状态机或绕过 core executor。

Endpoint 表单覆盖 `service_id`、`endpoint`、`protocol`、`health_path`、`display_name`、`note`、`config`。Link 表单覆盖 `source_endpoint`、`target_endpoint`、`protocol`、`auth_mode`、`scope`、`config_ref`、`secret_ref`、`policy`。`config` 和 `policy` 必须是 JSON；解析失败会返回校验错误，不会写入 Store。

`orchestrator/core/src/dispatcher.rs` 提供统一 Action Dispatcher。GUI 和 TUI 都只提交 `ActionRequest`，由 dispatcher 读取 action schema、生成 Operation、写 Store、调用固定 executor，并返回 `ActionDispatchResult`。结果必须标记能力状态：

```text
REAL         已做真实探测或真实读取，并写回可观测结果
STORE_BACKED 已写 Store、Operation 和 OperationLog，但外部执行能力有限
UNSUPPORTED  当前不能真实执行，必须写明原因，不能显示成功
READONLY     只读计算或查看，不改变 Service/Endpoint/Link/Topology
```

## Action 能力矩阵

| Action | GUI | TUI | 写 Store | 创建 Operation | Executor | 状态 | 证据 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| service.validate | 是 | 是 | 否 | 是 | 否 | READONLY | `action_dispatcher_routes_schema_actions` |
| service.install | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `OperationExecutor` 写 Service |
| service.start/stop/restart/enable/disable/delete | 是 | 是 | 只写失败 Operation/log | 是 | 否 | UNSUPPORTED | `action_result_marks_unsupported_without_success` |
| service.logs.view | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `operation_executor_materializes_operation_log_view_and_diagnostic_export` |
| service.health.check | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `operation_executor_persists_probed_endpoint_health` |
| set.validate | 是 | 是 | 否 | 是 | 否 | READONLY | `set_expand_apply_and_diagnostic_report_are_console_actions` |
| set.expand | 是 | 是 | 否 | 是 | 否 | READONLY | `set_expand_apply_and_diagnostic_report_are_console_actions` |
| set.apply | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `set_expand_apply_and_diagnostic_report_are_console_actions` |
| endpoint.register/update/delete | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `endpoint_register_update_delete_and_health_write_store` |
| endpoint.health.check | 是 | 是 | 是 | 是 | 是 | REAL | `endpoint_http_health_updates_store`、`endpoint_tcp_health_updates_store`、`endpoint_unreachable_is_recorded` |
| link.create/update/delete | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `link_create_update_delete_and_health_write_store` |
| link.health.check | 是 | 是 | 是 | 是 | 是 | REAL | `link_health_requires_existing_endpoints`、`link_health_uses_target_reachability` |
| operation.plan/confirm/apply/rollback | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `operation_plan_confirm_apply_rollback_and_logs_are_visible` |
| operation.logs.view | 是 | 是 | 否 | 否 | 否 | READONLY | `operation_plan_confirm_apply_rollback_and_logs_are_visible` |
| diagnostics.run/export | 是 | 是 | 是 | 是 | 是 | STORE_BACKED | `set_expand_apply_and_diagnostic_report_are_console_actions` |
| topology.load/validate/export | 是 | 是 | 否 | 是 | 否 | READONLY | `action_dispatcher_routes_schema_actions` |
| deployment.create、service.import、set.import、topology.apply | 是 | 是 | 只写失败 Operation/log | 是 | 否 | UNSUPPORTED | `unsupported_catalog_actions_never_enter_fake_success_path` |

DiagnosticReport 会包含 `action_matrix` 和 `unsupported_capabilities`，用于导出当前能力状态证据，避免把 STORE_BACKED 或 UNSUPPORTED 误写成 REAL。

执行能力由固定 driver 提供：

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

这些 driver 只接受固定 action。任意 shell、任意脚本路径、用户输入命令和远程 root shell 都不属于 Action 模型。
