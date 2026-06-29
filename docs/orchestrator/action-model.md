# Action 模型

GUI 和 TUI 使用同一套 Action Registry、Form Schema、Plan Schema、Result Schema 和 Error Schema。正式 action 来自 `orchestrator/schemas/actions.yaml`，并由 `orchestrator/core/src/action.rs` 校验。

Action 只能围绕：

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

当前正式 action 包括 service、set、endpoint、link、topology、operation 和 diagnostics 前缀下的固定动作。禁止引入 OJ 业务后台动作、Gateway 控制动作、Web Shell 管理动作、脚本动作、任意包动作或独立来源/产物对象动作。

ActionRequest 由 GUI/TUI 表单产生，但 Operation plan 必须由 core 生成。入口层不能自己拼装 plan、修改状态机或绕过 core executor。

执行能力由固定 driver 提供：

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

这些 driver 只接受固定 action。任意 shell、任意脚本路径、用户输入命令和远程 root shell 都不属于 Action 模型。
