# Operation 模型

Operation 是 Orchestrator 对核心对象执行变更或观测动作的审计单元。Operation 只作用于：

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

状态机为：

```text
PLANNED
AWAITING_CONFIRMATION
RUNNING
SUCCEEDED
FAILED
ROLLED_BACK
CANCELLED
EXPIRED
```

创建 plan 时会持久化 `operation_id`、`action`、`plan`、`status` 和 `created_at`。确认后写入 `confirmed_at`。apply 时先获取 `orchestrator_operation_locks`，再进入 `RUNNING`，按 plan step 写入 `orchestrator_operation_logs`，成功后写入 `result` 并进入 `SUCCEEDED`，失败后写入 `error_message` 并进入 `FAILED`。apply 完成后释放对应 lock。

rollback 会读取原 Operation 的 plan、result 和 logs，先记录已读取的历史日志数量，再把 `rollback_plan.steps` 写入 `orchestrator_operation_logs`，最后在原 Operation 上写入 `ROLLED_BACK` 与 `rolled_back_at`。当前选择是在原 Operation 上标记回滚，而不是创建新的 rollback Operation。

Executor 只支持固定 action，不执行任意 shell、任意脚本路径、用户输入命令或远程 root shell。当前固定 driver 为：

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

`LocalProcessDriver` 当前只允许健康检查和日志查看这类读动作；start/stop/restart 返回明确 Unsupported，直到接入安全 supervisor。

`DockerComposeDriver` 只构造固定 `docker compose` 子命令，如 `up -d`、`stop`、`restart`、`rm`、`logs`、`ps`，不恢复 scripts。默认模式只返回计划好的固定命令；显式启用执行模式后，core 通过参数数组调用固定 `docker compose` 命令，并把进程退出状态映射为 `SUCCEEDED` 或 `FAILED`。它仍不接受任意 shell、任意脚本路径或用户输入命令。

`ExternalEndpointDriver` 只管理既有 Endpoint 的 metadata、health、logs 和 reachability，不代表额外的主机、设备或安装实例模型。

GUI/TUI 通过 `OperationWorkbenchContext` 和 `OperationWorkbenchSession` 使用同一套状态机。GUI/TUI 可以生成 plan、confirm、apply、rollback，并查看 result、error 和 operation logs；不能绕过 core 自行执行动作。

当 `ORCHESTRATOR_DATABASE_URL` 存在时，`OperationWorkbenchContext` 会使用 `PgOrchestratorStore` 持久化工作台生成的 plan、confirm、apply 和 rollback。生成或更新 plan 时写入 `PLANNED` Operation；confirm 后写入 `AWAITING_CONFIRMATION` 和 `confirmed_at`；apply/rollback 继续由 `OperationExecutor` 写入 lock、step log、result、error 和最终状态。没有该变量时，工作台保持 `MemoryOrchestratorStore` 本地演示模式。

日志读取只围绕 `LogView` 和 `OperationLogRecord`。core 提供按 `service_id`、`endpoint`、`operation_id`、`source_id` 过滤的查询能力，并要求 `LogView.path` 使用 service-scoped、operation-scoped 或 endpoint-scoped 策略；它不是任意文件浏览器，也不读取未登记路径。

DiagnosticReport 可以从当前 Store 构建，内容包含 Service、Endpoint、Link、Operation 摘要、失败 Operation、不健康 Endpoint/Link、近期 Operation log、数据库 schema 检查和禁用概念扫描摘要。当前支持 JSON 和 Markdown 导出。

`run_reconcile_tick` 是当前长期运行能力的核心原语。它执行单次 tick：过期未确认 Operation、刷新 Endpoint/Link health、保存 Topology snapshot，并生成 DiagnosticReport。它可以被 GUI、TUI 或后续常驻进程调用，但本轮仍不宣称已经具备完整生产 daemon、远程部署 agent 或跨主机发布能力。
