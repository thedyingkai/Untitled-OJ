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
- Operation executor：`orchestrator/core/src/store.rs`
- Executor drivers：`orchestrator/core/src/executor.rs`
- Endpoint/Link health：`orchestrator/core/src/health.rs`
- HTTP health_path 检查测试：`tcp_probe_checks_http_health_path_status`
- LogView 查询与 DiagnosticReport 导出：`orchestrator/core/src/observability.rs`
- Reconcile tick：`orchestrator/core/src/reconciler.rs`
- GUI/TUI 共享视图：`orchestrator/core/src/view.rs`
- GUI/TUI 共享工作台：`orchestrator/core/src/workbench.rs`，未设置 `ORCHESTRATOR_DATABASE_URL` 时使用 Memory store，设置后 plan/confirm/apply/rollback 走 `PgOrchestratorStore`
- GUI/TUI Operation/LogView 观测：`orchestrator/core/src/view.rs` 从 Store 读取 Operation 与 OperationLog，GUI/TUI 展示 `operation_id`、状态、结果、错误、日志数量与日志消息
- Orchestrator migration：`deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql`

## 当前限制

- LocalProcessDriver 尚未接入安全 supervisor，因此生命周期动作返回 Unsupported。
- DockerComposeDriver 默认只返回固定命令计划；显式执行模式会运行固定 `docker compose` 参数数组，实际成功仍取决于本机 Docker/Compose 环境。
- 远程部署 agent、跨主机发布系统和完整生产 daemon 尚未实现。

## 最近本地验证

```powershell
cargo fmt --check
cargo check
cargo test

cd services/judge-worker
cargo fmt --check
cargo check
cargo test

cd services/shared
go test ./...

cd services/auth
go test ./...

cd services/gateway
go test ./...

cd services/problem-api
go test ./...

cd services/judge-api
go test ./...

cd frontend
npm ci --registry=https://registry.npmjs.org --replace-registry-host=always
npm audit --registry=https://registry.npmjs.org --audit-level=high
npm run build
```

结果：通过。`frontend/dist` 为构建产物，验证后已删除，不进入提交。
