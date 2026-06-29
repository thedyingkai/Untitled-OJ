# Operation 模型

Operation 是 Orchestrator 对核心对象执行的可审计动作。Operation 只作用于：

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

状态机：

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

规则：

- 所有危险操作必须先生成 plan。
- 用户确认后才能 apply。
- apply 过程必须记录 operation log。
- 成功后写入 result。
- 失败后写入 error_message。
- 可回滚操作必须保存 rollback_plan。
- secret、token、密码和连接凭据必须在日志、计划摘要和错误信息中脱敏。

`orchestrator/core` 统一生成 `service.install`、Service 启停、`set.apply`、`endpoint.register`、`link.create` 和 `topology.apply` 的 Operation 计划。GUI 与 TUI 只展示并提交这些计划，不能绕过 core 自行拼装执行路径。

Operation 不是 OJ 业务流程。提交评测、题目管理、比赛流程和用户权限业务不进入 Orchestrator Operation。

Action Catalog 会把每个 action 标注为只读、直接执行、生成计划或计划并确认。Operation Executor 根据 plan 中的 `requires_confirmation` 判断是否允许直接 apply；高风险 action 必须要求确认。

Operation Executor 不只是状态切换器。它会依据 Operation 的 action、request 和 plan 更新 core store 中的正式对象：`service.install` 写入 Service，`set.apply` 写入 Set 并声明默认 Endpoint/Link，`endpoint.register` 写入 Endpoint，`link.create` 写入 Link，`topology.apply` 写入 Topology、LogView 和 DiagnosticReport。只读或诊断类动作仍通过 Operation 日志和 result 留痕，不进入 OJ 业务数据。

计划预览由 `orchestrator/core` 根据 ActionRequest、Service、Set、Endpoint、Link 和当前 Topology 生成。预览只暴露目标对象、步骤摘要、确认要求和回滚可用性，不把 secret、token、密码或连接凭据写入展示文本。

Operation 工作台是 GUI/TUI 共用的 core 模型。它持有当前 ActionRequest、表单字段、Operation、计划预览、确认/执行/回滚能力和告警摘要。工作台可以通过 core executor 跑通确认、执行、回滚流，并返回日志和结果摘要；GUI/TUI 只负责展示和提交用户交互，不拥有独立执行语义。

工作台会话支持四类状态动作：字段更新、确认、执行、回滚。字段更新会重新生成 Operation 与 preview；确认只允许从 `PLANNED` 进入 `AWAITING_CONFIRMATION`；执行必须遵守 plan 的 `requires_confirmation`；回滚只能针对已成功或失败且带 rollback plan 的 Operation。所有这些规则都在 `orchestrator/core` 中执行。

`OperationWorkbenchContext` 负责加载当前仓库上下文：共享 schema、Service、Set、Endpoint、Link 和 Topology 预览。GUI/TUI 不直接拼装这些输入，而是从 context 创建 session，再把用户选择和字段值交回 context。这样 GUI/TUI 可以有不同交互形态，但 Operation 状态机、计划生成和执行结果始终来自同一条 core 路径。

context 执行 `apply` 和 `rollback` 时会先把当前 Service、Set、Endpoint、Link 和 Topology 放入 core store，再交给 Operation Executor。`endpoint.register`、`link.create`、`set.apply` 这类依赖上下文的 action 因此可以使用真实的 core 对象关系完成校验和变更，而不是在空 store 中做状态模拟。

回滚会按 Operation 类型撤销可证明的 core store 变更：新安装 Service、注册 Endpoint、新建 Link 和应用 Topology 可以移除对应对象；删除类操作目前记录回滚结果和日志，但恢复原对象需要执行前快照。需要恢复完整旧状态的操作必须在 `rollback_plan` 中携带可核对快照。
