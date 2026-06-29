# Action 模型

GUI 和 TUI 使用同一套 Action Registry、Form Schema、Plan Schema、Result Schema 和 Error Schema。正式 Action Registry 来自 `orchestrator/schemas/actions.yaml`。

当前正式 action 为：

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

Action 只能围绕 Service、Set、Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport。站点业务后台、流量代理控制、构建产物、仓库来源、额外主机对象或安装实例对象都不能作为正式 action 进入 registry。

每个 action 必须有对应表单定义、计划结构、结果结构和错误结构。GUI/TUI 只能解释这些 schema，不能各自维护不同的业务动作。

`orchestrator/core` 维护正式 Action Catalog。Catalog 为每个 action 绑定目标核心对象、风险等级、执行模式、计划要求和摘要说明。`orchestrator/schemas/actions.yaml` 仍是 GUI/TUI 共用的动作清单，但它必须与 core catalog 完全一致，加载共享 schema 时会同步校验。

`orchestrator/core` 同时提供 ActionRequest 到 Operation 的计划预览入口和 Operation 工作台模型。GUI/TUI 可以提交同一份 action 表单输入，由 core 返回目标对象、步骤摘要、确认要求、回滚可用性和执行流摘要；GUI/TUI 不能绕过 core 自己拼装计划。core 还提供默认 ActionRequest，用于 GUI/TUI 表单初始化和预览，不是 CLI、脚本或新的产品入口。

`OperationWorkbenchContext` 是 core 内部的共享加载上下文。它从仓库读取 `orchestrator/schemas/*`、`services/*/service.yaml` 和 `sets/*.yaml`，并派生 Endpoint、Link 和 Topology 预览。GUI/TUI 只持有这个上下文和当前 `OperationWorkbenchSession`，不在入口层重复加载、解释或执行 action 语义。

工作台支持字段更新。GUI/TUI 修改表单字段后，必须把字段值交回 core，由 core 重新生成 ActionRequest、Operation、preview 和确认要求。入口层不能局部修改 preview，也不能跳过 core 直接改变 Operation 状态。TUI 使用 core 提供的字段候选值循环能力实现键盘编辑；GUI 使用文本输入控件提交字段值；两者最终都调用同一个 core 更新函数。

风险与执行模式：

- 低风险只读动作不生成变更。
- 低风险检查动作可以直接执行并写入诊断结果。
- 中风险动作必须生成 Operation plan。
- 高风险动作必须生成 Operation plan，并经过确认后才能 apply。

当前 `orchestrator/core` 已提供 Service 启停删除、Service 日志和健康检查、Set 应用、Endpoint 注册更新删除和健康检查、Link 创建更新删除和健康检查、Topology 应用，以及 Operation 确认、取消、执行、回滚和过期的统一模型。GUI/TUI 使用同一份 Operation 工作台摘要展示当前 action、结构化表单字段、预览步骤、确认要求、当前状态、结果状态、日志计数和回滚能力；action 选择、字段更新、确认、执行和回滚都通过 core session 完成。
