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

GUI/TUI 的 `OperationWorkbenchContext` 会根据 `ORCHESTRATOR_DATABASE_URL` 选择 Store。未设置时使用 `MemoryOrchestratorStore` 做无数据库演示；设置后，工作台先通过 `PgOrchestratorStore` 读取当前 Service、Set、Endpoint、Link 和 Topology，再把 plan/confirm/apply/rollback 持久化为 Operation、operation logs、result、rollback 和相关核心对象。GUI/TUI 仍不直接写数据库，只通过 core Store trait。

`OrchestratorActionConsole` 在无数据库时持有会话级 `MemoryOrchestratorStore`，因此 GUI/TUI 操作后的 Endpoint、Link、Operation 和 LogView 会在本次会话刷新后继续可见。设置 `ORCHESTRATOR_DATABASE_URL` 时，Action Console 通过 `PgOrchestratorStore` 写入独立 Orchestrator DB；它不会读取或写入 `OJ_DATABASE_URL`。

PostgreSQL Store 集成测试位于 `orchestrator/core/tests/pg_store_integration.rs`。普通 `cargo test` 只编译该测试；需要真实数据库时，先对独立 Orchestrator DB 应用 `deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql`，再运行：

```powershell
$env:ORCHESTRATOR_DATABASE_URL="postgres://postgres:ojos-orchestrator-local@localhost:5432/ojos_orchestrator?sslmode=disable"
cargo test -p orchestrator-core --test pg_store_integration -- --ignored
```

该测试会通过 `PgOrchestratorStore` 写入并读回 Service、Set、Endpoint、Link、Operation、OperationLog、TopologySnapshot、LogView 和 DiagnosticReport，用来证明 Store 映射不是只停留在 schema 扫描。

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

Operation log 使用 `orchestrator_operation_logs`，按 `operation_id` 和 `step_id` 记录执行过程，结构化数据写入 `data`。内存 Store 会给本地 operation log 写入 `log-1`、`log-2` 这类确定性 marker，PostgreSQL Store 使用数据库 `created_at` 排序并读回。

core 内部的 `confirmed`、`started`、`finished`、`failed`、`rolled_back` 状态 marker 在写入 PostgreSQL 时映射为数据库侧 `now`，因此 `confirmed_at`、`started_at`、`finished_at`、`rolled_back_at` 会落成真实时间戳；`session` 这类非持久锁 marker 仍走数据库默认过期策略。

Operation lock 使用 `orchestrator_operation_locks`，字段为 `lock_key`、`operation_id`、`owner`、`expires_at`、`created_at`。锁表名称保持完整前缀，不使用泛名 `operation_locks`。

LogView 使用 `log_sources`，字段为 `source_id`、`endpoint`、`service_id`、`operation_id`、`kind`、`path`、`driver`、`read_policy`、`created_at`、`updated_at`。`endpoint` 必须是 `IP:Port`，`operation_id` 可为空，`read_policy` 必须是 service-scoped、endpoint-scoped 或 operation-scoped。

Topology snapshot 使用 `snapshot_id`、`topology`、`created_at`。Topology 内容仍然只围绕 Endpoint 和 Link，不引入额外运行实例对象。
