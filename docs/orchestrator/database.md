# 编排器数据库

`ORCHESTRATOR_DATABASE_URL` 只指向编排器自己的 PostgreSQL 数据库。auth、problem、judge、user 等服务分别
使用 `AUTH_DATABASE_URL`、`PROBLEM_DATABASE_URL`、`JUDGE_DATABASE_URL`、`USER_DATABASE_URL`。业务服务
不直接写编排器表，编排器也不写业务表。

`services/orchestrator/migrations/000001_orchestrator_schema.up.sql` 当前创建 21 张表：

```text
service_releases
host_services
service_endpoints
service_links
service_routes
service_migration_records
service_permission_records
service_frontend_entries
service_redis_resources
service_storage_resources
rendered_service_configs
nodes
service_api_surfaces
deployed_service_apis
services
orchestrator_operations
orchestrator_operation_logs
orchestrator_operation_locks
topology_snapshots
log_sources
diagnostic_reports
```

表可按用途分成四组：

- 服务注册：release、service、host service、endpoint、link、route 和各类 release 资源记录。
- API 解析：`nodes`、`service_api_surfaces`、`deployed_service_apis`。有效 API 路由由这些表派生，不另建表。
- 操作审计：Operation、OperationLog 和 OperationLock。
- 视图与诊断：Topology 快照、日志源和诊断报告。

Endpoint 和 Link 两端都使用 `ip:port:service-name`。`service_id` 是兼容字段，必须与 Endpoint 中的
service-name 一致。`judge-worker[*]` 这类值是按 `service_name` 查询运行端点的表达式，不是数据库对象。

未设置 `ORCHESTRATOR_DATABASE_URL` 时，三个入口使用 `MemoryOrchestratorStore`。数据只保存在当前进程，
重启后会丢失。配置了 URL 但数据库不可用时会返回错误，不会静默降级到内存。

当前 `PgOrchestratorStore` 使用 `postgres::Client` 和 `NoTls`，每次通过 console 打开连接，没有连接池。生产
部署需要在可信网络或外部 TLS 代理后使用，并把连接池化列为容量上线前的改进项。
