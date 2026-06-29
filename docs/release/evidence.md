# 可核对证据

本文件记录 OJOS Orchestrator 当前正式结构的人工可核对证据。它不是脚本验收清单，也不把自动化命令当作唯一完成依据；每一项都指向可直接检查的文件、模型或测试覆盖。

## 正式目录

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

历史迁移内容位于 `docs-temp/`，不作为当前正式架构来源。

当前正式文档文件树：

```text
docs/architecture/README.md
docs/orchestrator/action-model.md
docs/orchestrator/boundary.md
docs/orchestrator/database.md
docs/orchestrator/gui-tui-parity.md
docs/orchestrator/operation-model.md
docs/orchestrator/requirements.md
docs/orchestrator/topology-model.md
docs/release/README.md
docs/release/evidence.md
docs/services/README.md
docs/spec/endpoint-link-spec.md
docs/spec/service-spec.md
docs/spec/set-spec.md
```

`docs-temp/` 保存旧架构、旧发布、旧脚本和旧文档索引。它允许出现历史名称，但不能被 README、正式 spec、Orchestrator 文档或 release 文档引用为当前架构来源。

## 核心对象证据

核心对象集合由 `orchestrator/core/src/action.rs`、`orchestrator/core/src/model.rs`、`orchestrator/core/src/store.rs` 和 `orchestrator/core/src/view.rs` 共同约束：

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

`orchestrator/core/src/action.rs` 使用正式 action 前缀白名单，`orchestrator/schemas/actions.yaml` 与 core Action Catalog 必须一致。GUI/TUI 只读取同一套 schema 和 core view，不各自维护另一套编排动作。

正式 Action Registry：

```text
deployment.create
deployment.open
deployment.diagnose
service.import
service.validate
service.install
service.enable
service.disable
service.start
service.stop
service.restart
service.delete
service.logs.view
service.health.check
set.import
set.validate
set.expand
set.apply
set.compare
endpoint.register
endpoint.update
endpoint.delete
endpoint.health.check
link.create
link.update
link.delete
link.health.check
topology.load
topology.validate
topology.apply
topology.export
operation.plan
operation.confirm
operation.apply
operation.cancel
operation.rollback
operation.logs.view
diagnostics.run
diagnostics.export
```

共享 schema 文件必须且只包含：

```text
orchestrator/schemas/actions.yaml
orchestrator/schemas/forms.yaml
orchestrator/schemas/plans.yaml
orchestrator/schemas/results.yaml
orchestrator/schemas/errors.yaml
```

GUI / TUI 等价性矩阵：

| 能力 | GUI | TUI | 共同来源 |
| --- | --- | --- | --- |
| Service 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| Set 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| Endpoint 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| Link 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| Operation 工作台 | 有 | 有 | `orchestrator/core/src/workbench.rs` |
| Topology 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| LogView 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| DiagnosticReport 视图 | 有 | 有 | `orchestrator/core/src/view.rs` |
| 表单 schema | 有 | 有 | `orchestrator/schemas/forms.yaml` |
| 结果 schema | 有 | 有 | `orchestrator/schemas/results.yaml` |
| 错误 schema | 有 | 有 | `orchestrator/schemas/errors.yaml` |

## 数据库证据

Orchestrator migration 位于：

```text
deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql
```

正式表只有：

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

OJ 业务 migration 位于：

```text
deploy/oj-migrations/
```

`deploy/compose/docker-compose.yml` 使用独立的 `orchestrator-db` 与 `postgres`，并分别通过 `ORCHESTRATOR_DATABASE_URL` 和 `OJ_DATABASE_URL` 执行 migration。业务 Service 只接收 `OJ_DATABASE_URL`，不接收 `ORCHESTRATOR_DATABASE_URL`。

数据库边界核对点：

| 项目 | 证据 |
| --- | --- |
| Orchestrator 数据库 | Compose 中的 `orchestrator-db` |
| OJ 业务数据库 | Compose 中的 `postgres` |
| Orchestrator migration | `deploy/orchestrator-migrations/` |
| OJ migration | `deploy/oj-migrations/` |
| Orchestrator URL | `ORCHESTRATOR_DATABASE_URL` |
| OJ URL | `OJ_DATABASE_URL` |
| Orchestrator 表访问 | `orchestrator/core/src/database.rs` 只列出正式表 |

## Service / Set 证据

基础 Service 的正式契约文件：

```text
services/auth/service.yaml
services/gateway/service.yaml
services/judge-api/service.yaml
services/judge-worker/service.yaml
services/postgres/service.yaml
services/problem-api/service.yaml
services/redis/service.yaml
services/storage/service.yaml
services/web-shell/service.yaml
```

Service 覆盖表：

| Service | kind | 契约 |
| --- | --- | --- |
| auth | backend-api | `services/auth/service.yaml` |
| gateway | gateway | `services/gateway/service.yaml` |
| judge-api | backend-api | `services/judge-api/service.yaml` |
| judge-worker | backend-worker | `services/judge-worker/service.yaml` |
| postgres | database | `services/postgres/service.yaml` |
| problem-api | backend-api | `services/problem-api/service.yaml` |
| redis | cache | `services/redis/service.yaml` |
| storage | storage | `services/storage/service.yaml` |
| web-shell | frontend | `services/web-shell/service.yaml` |

正式 Set：

```text
sets/single-node-oj.yaml
sets/distributed-oj.yaml
sets/judge-worker-node.yaml
sets/course-judge.yaml
sets/service-development.yaml
```

Set 只描述 Service、默认 Endpoint、默认 Link、安装顺序、启动顺序和部署策略，不提供运行时 API，也不包含 OJ 业务逻辑。

Set 覆盖表：

| Set | 文件 | 作用 |
| --- | --- | --- |
| single-node-oj | `sets/single-node-oj.yaml` | 单机 OJ 组合 |
| distributed-oj | `sets/distributed-oj.yaml` | 分布式 OJ 组合 |
| judge-worker-node | `sets/judge-worker-node.yaml` | 受限评测节点组合，不包含 web-shell |
| course-judge | `sets/course-judge.yaml` | 课程训练评测组合 |
| service-development | `sets/service-development.yaml` | Service 开发验证组合 |

## Gateway / Web Shell 边界证据

Gateway 的 Orchestrator 管理视图路由位于 `services/gateway/internal/handler/routes.go`，只暴露只读 `GET` 和 `OPTIONS`。`services/gateway/internal/handler/routes_test.go` 检查 Orchestrator 管理路由必须保持只读。

Gateway 读取 Orchestrator snapshot 和 routing 信息的实现位于：

```text
services/gateway/internal/orchestrator/snapshot/
services/gateway/internal/orchestrator/servicestatus/
```

Web Shell 的 Service、Topology、Status 和 Snapshot 页面位于 `frontend/src/views/admin/`，这些页面只读取 Gateway 暴露的只读 API，不安装 Service、不修改 Endpoint / Link、不 apply Topology、不执行 Operation。

边界核对表：

| 对象 | 允许 | 禁止 | 证据 |
| --- | --- | --- | --- |
| Gateway | 读取 snapshot、认证、权限、审计、限流、代理业务流量 | 安装 Service、写 Endpoint、写 Link、写 Topology、执行 Operation、bootstrap registry | `services/gateway/internal/handler/routes_test.go`、`services/gateway/internal/orchestrator/` |
| Web Shell | 读取 Service、Topology、Status、Snapshot 的只读视图，承载 OJ 业务页面 | 充当 Orchestrator、安装 Service、管理 Endpoint/Link、修改 Topology、执行 Operation | `frontend/src/router/index.ts`、`frontend/src/views/admin/` |

## 基础验证证据

当前基础验证命令：

```powershell
cargo fmt --check
cargo check
cargo test
```

```powershell
cd services\shared; go test ./...; cd ..\..
cd services\auth; go test ./...; cd ..\..
cd services\gateway; go test ./...; cd ..\..
cd services\problem-api; go test ./...; cd ..\..
cd services\judge-api; go test ./...; cd ..\..
```

```powershell
cd frontend
npm ci --registry=https://registry.npmjs.org --replace-registry-host=always
npm audit --registry=https://registry.npmjs.org --audit-level=high
npm run build
cd ..
```

这些命令只证明代码层基础检查通过。最终发布仍必须同时核对正式文档、核心对象模型、数据库 schema、GUI/TUI 等价性、Service/Set 样例、Endpoint/Link 行为、Gateway/Web Shell 边界、污染扫描和人工变更报告。

最近一次本地验证结果：

| 类别 | 结果 |
| --- | --- |
| Rust workspace | `cargo fmt --check`、`cargo check`、`cargo test -p orchestrator-core` 通过 |
| GUI / TUI | `cargo test -p ojos-orchestrator-gui -p ojos-orchestrator-tui` 通过 |
| Judge Worker | `cargo fmt --check`、`cargo check`、`cargo test` 通过 |
| Go | `services/shared`、`services/auth`、`services/gateway`、`services/problem-api`、`services/judge-api` 的 `go test ./...` 通过 |
| Frontend | `npm ci`、`npm audit --audit-level=high`、`npm run build` 通过 |
| Compose | `docker compose --env-file .env.example -f deploy/compose/docker-compose.yml config` 通过 |

这些结果不能替代人工核对，但可以作为编译和模型测试证据。

## 污染证据

仓库不应跟踪以下内容：

```text
.env
.tmp/
tmp/
target/
frontend/dist/
frontend/node_modules/
node_modules/
*.ojosmod
*.ojossvc
*.log
compose-logs.txt
compose-ps.txt
tokens.local.json
```

发布前必须核对无真实 secret、无本机路径泄露、无 tracked 构建产物或本地依赖目录。
