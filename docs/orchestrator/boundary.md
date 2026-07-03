# 编排器边界

OJOS Orchestrator 是服务编排器，不是 OJ 业务后端，也不是 Gateway 前端。

## Gateway

Gateway 是一个 Service。它负责业务流量、认证中间件、请求路由、统一错误和健康上报。

Gateway 不安装服务、不管理 endpoint 或 link、不改动拓扑、不执行 operation，也不充当控制平面。

## Gateway 前端

Gateway 前端是 OJ 站点 UI：题目、提交、判题结果，以及普通管理视图。

Gateway 前端不安装服务、不管理 endpoint 或 link、不改动拓扑、不执行 operation，也不充当编排器。

## Orchestrator daemon

Orchestrator daemon 是编排器的 HTTP API 入口。

它可以：

```text
读取 ORCHESTRATOR_DATABASE_URL，或使用本地内存 store 上下文
暴露 service release、service、endpoint、link、operation、topology、log 和 diagnostic API
把写请求转换为 core 的 ActionRequest 值
把执行委托给 OrchestratorActionDispatcher
读取 operation 状态、operation 日志、拓扑和诊断报告
```

它不得代理 OJ 业务流量、不得服务 Gateway 前端页面、不得执行任意 shell、不得绕过 GUI/TUI 的 core
action schema，也不得引入额外的运行时实例对象。

## Root 角色

Root 是编排器的一种运行时角色，不是独立的 rootd 程序。Node 与 standalone 也是同一个编排器程序的角色。

Root 信息由配置、授权策略和拓扑起点表示：

```text
topology.root_host
topology.root_endpoint
authority.root_host
authority.root_endpoint
authority.exposure_policy
```

## OJ 业务边界

题目、提交、比赛、用户、权限业务逻辑、公告、训练、Clarification、打印和滚榜，属于被管理的服务内部，
不属于编排器 core。
