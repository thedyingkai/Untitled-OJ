# Orchestrator 数据库

OJOS Orchestrator 使用独立数据库。Orchestrator DB 只保存 Service、Set、Endpoint、Link、Operation、Topology、LogView、DiagnosticReport 相关数据；OJ DB 只保存题库、提交、比赛、用户、权限等 OJ 业务数据。

数据库连接必须分离：

```text
ORCHESTRATOR_DATABASE_URL -> Orchestrator DB
OJ_DATABASE_URL           -> OJ business DB
```

业务 Service 不能直接写 Orchestrator DB。Orchestrator 不能直接写 OJ 业务表。

正式 Orchestrator 表只有：

```text
service_sets
services
service_endpoints
service_links
orchestrator_operations
orchestrator_operation_logs
orchestrator_operation_locks
topology_snapshots
log_sources
diagnostic_reports
```

`deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql` 是当前正式初始化链路。它不创建旧主机、设备、安装实例、包、运行时操作或旧模块相关表。

`orchestrator/core/src/store.rs` 定义 `OrchestratorStore` trait。它覆盖 Service、Set、Endpoint、Link、Operation、OperationLog、TopologySnapshot、LogView 和 DiagnosticReport 的 list/get/upsert/delete、health 更新、operation 状态更新、operation lock、拓扑快照、日志源和诊断报告能力。

`MemoryOrchestratorStore` 只用于测试、本地开发和无数据库演示。它不是生产 Store。

`PgOrchestratorStore` 位于 `orchestrator/core/src/database.rs`，只从 `ORCHESTRATOR_DATABASE_URL` 连接 Orchestrator DB，并把 Store 行为映射到上面的正式表。它不读取 `OJ_DATABASE_URL`，也不访问 OJ migration 创建的业务表。

Operation 持久化字段包括：

```text
operation_id
action
status
plan
result
error_message
created_at
updated_at
confirmed_at
started_at
finished_at
rolled_back_at
```

Operation log 使用 `orchestrator_operation_logs`，按 `operation_id` 和 `step_id` 记录执行过程，结构化数据写入 `data`。

Operation lock 使用 `orchestrator_operation_locks`，字段为 `lock_key`、`operation_id`、`owner`、`expires_at`、`created_at`。锁表名称保持完整前缀，不使用泛名 `operation_locks`。

Topology snapshot 使用 `snapshot_id`、`topology`、`created_at`。Topology 内容仍然只围绕 Endpoint 和 Link，不引入额外运行实例对象。
