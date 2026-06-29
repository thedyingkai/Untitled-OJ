# Orchestrator 边界

OJOS Orchestrator 是服务编排器，不是 OJ 业务后台，也不是 Web 控制面。

## Gateway

Gateway 是 Service。它负责：

- 业务流量入口。
- 认证、鉴权、审计、限流和统一错误。
- 读取 Orchestrator 输出的 routing snapshot。
- 根据 Endpoint / Link 代理业务请求。
- 上报自身健康状态。

Gateway 不安装 Service，不管理 Endpoint / Link，不修改 Topology，不执行 Operation，也不成为控制面。

## Web Shell

Web Shell 是 Service。它负责 OJ 站点前后端业务 UI，例如题库、提交、评测结果和普通管理视图。

Web Shell 不安装 Service，不管理 Endpoint / Link，不修改 Topology，不执行 Operation，也不充当 Orchestrator。

## Orchestrator daemon

Orchestrator daemon 是编排器的 HTTP API 入口。它只负责：

- 读取 `ORCHESTRATOR_DATABASE_URL` 或使用本地 Store 上下文。
- 提供 Service、Set、Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport 的 Orchestrator API。
- 把写操作转换为 core `ActionRequest`，并交给 `OrchestratorActionDispatcher` 执行。
- 查询 Operation 状态、日志、Topology 和诊断结果。

Orchestrator daemon 不代理 OJ 业务，不提供 Web Shell 页面，不执行任意 shell，不绕过 GUI/TUI 使用的 core action schema，也不引入额外运行实例对象。

## root host

root host 不是额外核心对象。root 信息由 Orchestrator 配置、authority 策略和 Topology 起点表达：

```text
topology.root_host
topology.root_endpoint
authority.root_host
authority.root_endpoint
authority.exposure_policy
```

完整 GUI/TUI 只暴露在 root host。普通 host 不允许自行修改全局 Topology、创建全局 Link 或提升为 root。

## OJ 业务边界

题库、提交、比赛、用户、权限业务后台、公告、训练、Clarification、打印和滚榜都属于被编排 Service 的内部业务，不进入 Orchestrator 核心对象。
