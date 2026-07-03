# 编排器数据库

OJOS 使用分布式的 service-owned 数据库。`ORCHESTRATOR_DATABASE_URL` 指向编排器数据库。业务服务使用各自
的数据库 URL，如 `AUTH_DATABASE_URL`、`PROBLEM_DATABASE_URL`、`JUDGE_DATABASE_URL`、`USER_DATABASE_URL`；
它们不得共用一个 OJ 业务数据库。业务服务不得直接写编排器数据库，编排器也不得写 service-owned 的业务表。

正式编排器表：

```text
service_releases
host_services
services
service_endpoints
service_links
service_routes
service_migration_records
orchestrator_operations
orchestrator_operation_logs
orchestrator_operation_locks
topology_snapshots
log_sources
diagnostic_reports
```

Endpoint 运行时身份是 `ip:port:service-name`。`service_id` 作为兼容字段保留，且必须与内嵌的 service-name
一致。Link 的 source 与 target endpoint 字段使用相同的 `ip:port:service-name` 身份。像 `judge-worker[*]`
这样的值是按 `service_name` 对运行中 endpoint 的派生查询，不是正式编排器表。

`services/orchestrator/migrations/000001_orchestrator_schema.up.sql` 是正式 schema 初始化脚本。它不得重建
已废弃的 machine、device、installer、service installation 或 runtime-manager 表。

`service_routes` 存储路由声明。`service_migration_records` 跟踪 `service_name + migration_version`。
`log_sources` 存储 LogView 元数据，含独立的 `endpoint` 和 `service_id` 字段。
