# Orchestrator 数据库

OJOS Orchestrator 使用独立数据库。编排器数据库只保存 Service、Set、Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport 相关数据；OJ 业务数据库保存用户、权限、题目、提交、评测任务等业务数据。

部署配置必须分离：

```text
ORCHESTRATOR_DATABASE_URL -> Orchestrator 数据库
OJ_DATABASE_URL           -> OJ 业务数据库
```

正式 Orchestrator 表只有：

```text
services
service_sets
service_endpoints
service_links
orchestrator_operations
orchestrator_operation_logs
orchestrator_operation_locks
topology_snapshots
log_sources
diagnostic_reports
```

初始化链路只能创建上面的表。任何不属于 Service、Set、Endpoint、Link、Operation、Topology、LogView 或 DiagnosticReport 的对象表都不得进入 Orchestrator 正式 schema。

当前 Orchestrator migration 位于：

```text
deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql
```

`orchestrator/core/src/database.rs` 维护正式表清单、非正式表检查、migration 检查、数据库访问语句清单和写入计划模型。数据库访问语句只触碰上面的 10 张 Orchestrator 表，用于 Service、Set、Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport 的持久化边界；它不读写 OJ 业务表。

`orchestrator/core/src/store.rs` 定义当前 core store 接口。内存实现用于 GUI/TUI 工作台、单元测试和可核对的编排行为；接口覆盖 Service、Set、Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport 的写入、删除、查询和枚举。后续数据库实现必须保持同一对象边界，不能增加额外主机、设备、安装实例或包格式对象表。

OJ 业务 migration 位于：

```text
deploy/oj-migrations/
```

`service_endpoints.endpoint` 是主键，格式为 `IP:Port`。`service_links` 使用 `(source_endpoint, target_endpoint)` 作为主键。

`orchestrator_operations` 只记录编排动作，不记录 OJ 提交、题库或比赛业务流程，并为可回滚操作保存 `rollback_plan`。

Operation Executor 会把可执行计划落到 store：安装 Service、应用 Set、注册 Endpoint、创建 Link、应用 Topology、写 LogView 和 DiagnosticReport 都通过同一套 store 接口完成。数据库实现接入时应把这些 store 行为映射到上面的正式表，而不是绕过 core 直接写业务库。

`plan_database_writes` 可以从当前 store 生成持久化写入计划。计划不会连接数据库，只说明每个核心对象要写入的正式表和对应 SQL statement 名称，用于核对持久化边界：Service -> `services`，Set -> `service_sets`，Endpoint -> `service_endpoints`，Link -> `service_links`，Operation -> `orchestrator_operations`，Operation 日志 -> `orchestrator_operation_logs`，Topology -> `topology_snapshots`，LogView -> `log_sources`，DiagnosticReport -> `diagnostic_reports`。`orchestrator_operation_locks` 保留给执行互斥，不由普通对象写入计划直接生成业务记录。

`topology_snapshots` 保存 `root_host`、`root_endpoint`、`authority` 和 `exposure_policy`。`root_host` 是 `root_endpoint` 的 IP 部分，用于表达 root authority 和完整 GUI/TUI exposure，不引入额外主机对象表或设备对象表。
