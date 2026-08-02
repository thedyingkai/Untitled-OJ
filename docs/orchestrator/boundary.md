# 编排器边界

OJOS Orchestrator 管理服务，不承载 OJ 业务请求。

## Gateway

Gateway 是一个被管理的 Service，负责业务流量、认证中间件、请求路由、统一错误和健康上报。

Gateway 不安装服务、不管理 endpoint 或 link、不改动拓扑、不执行 operation，也不充当控制平面。

## Gateway 前端

Gateway 前端是 OJ 站点 UI：题目、提交、判题结果，以及普通管理视图。

Gateway 前端不安装服务、不管理 endpoint 或 link、不改动拓扑、不执行 operation，也不充当编排器。

## Orchestrator daemon

Orchestrator daemon 提供控制面 HTTP API，并托管 `manager/web/dist`。它不会托管 Gateway frontend。

它可以：

```text
读取 ORCHESTRATOR_DATABASE_URL，未设置时使用内存 store
暴露 service release、service、endpoint、link、operation、topology、log 和 diagnostic API
把写请求转换为 core 的 ActionRequest 值
把执行委托给 OrchestratorActionDispatcher
读取 operation 状态、operation 日志、拓扑和诊断报告
```

daemon 不代理 OJ 业务流量，也不接受任意 shell 或用户提供的命令。`LocalProcessDriver` 和
`DockerComposeDriver` 只能执行发布契约中声明的固定运行方式，而且请求必须显式设置
`execute_service_driver=true`。Web UI、TUI 和 HTTP 调用都不能绕过 core action schema。

## Root 角色

Root 是编排器节点角色，不是独立的 rootd 程序。Node 与 standalone 也是同一个程序的角色。

Root 信息由配置、授权策略和拓扑起点表示：

```text
topology.root_host
topology.root_endpoint
authority.root_host
authority.root_endpoint
authority.exposure_policy
```

## OJ 业务边界

题目、提交、比赛、用户、权限业务逻辑、公告、训练、Clarification、打印和滚榜属于业务 Service，不进入
编排器 core 或数据库。
