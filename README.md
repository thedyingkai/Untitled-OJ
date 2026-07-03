# OJOS Orchestrator

OJOS Orchestrator（OJOS 编排器）是面向 OJOS 服务体系的服务编排产品。它负责导入、校验、规划、安装、连接、启停、观测和诊断 Service。

它不是 OJ 网站后台，也不实现题库、提交、比赛、用户、公告、训练、Clarification、打印或滚榜。这些能力都属于具体 Service；编排器只处理 Service 之间的安装计划、连接关系、运行状态和拓扑视图。

## 核心对象

正式核心对象只有：

- Service：最小可安装、可启停、可连接、可观测的功能单元。
- Set：推荐部署组合，只描述组成和默认关系，不作为运行时对象。
- Endpoint：运行中 Service 的唯一连接身份，格式固定为 `ip:port:service-name`。
- Link：`source endpoint -> target endpoint` 的通信授权关系。
- Operation：需要计划、确认、执行、记录和回滚的编排动作。
- Topology：Service、Set、Endpoint、Link、Operation、LogView、DiagnosticReport 的关系视图。
- LogView：按 Service、Endpoint 或 Endpoint host/IP 聚合的日志视图。
- DiagnosticReport：部署和运行诊断结果。

## 正式入口

正式入口包括 Orchestrator GUI、Orchestrator TUI 和 Orchestrator daemon。三者使用同一套 `services/orchestrator/core` 和 `platform/schemas/orchestrator`，能力必须一致，差别只能是交互形态或传输形态。

Gateway 是业务流量入口 Service，不是控制面。Gateway frontend 是 OJ 业务 UI，不是 Orchestrator 入口。

## 数据库边界

Orchestrator 使用独立数据库：

```text
ORCHESTRATOR_DATABASE_URL
```

OJ 业务服务使用各自的服务级数据库：

```text
AUTH_DATABASE_URL
PROBLEM_DATABASE_URL
JUDGE_DATABASE_URL
USER_DATABASE_URL
```

Orchestrator 不写 service-owned 业务表；OJ 业务服务也不能直接写 Orchestrator 表。

## 基础 Service

当前基础 Service 包括：

```text
gateway
auth-service
problem-service
user-service
judge-api
judge-worker
postgresql
redis
storage-service
minio
jaeger
orchestrator
```

每个 Service 必须提供 `service.yaml` 和相邻 `release.yaml`。`service.yaml` 是正式 Service 身份契约；`release.yaml` 是发布/导入契约，并且 route、version、backend protocol/port 必须与 `service.yaml` 对齐。

## 文档

正式文档位于 `docs/`，完整索引见 [docs/README.md](docs/README.md)：

- [文档索引](docs/README.md)
- [项目完成度总结](docs/completeness-summary.md)
- [未完成事项](docs/unfinished/README.md)
- [Service 规范](docs/spec/service-spec.md)
- [Set 规范](docs/spec/set-spec.md)
- [Endpoint / Link 规范](docs/spec/endpoint-link-spec.md)
- [Orchestrator 需求](docs/orchestrator/requirements.md)
- [Orchestrator 边界](docs/orchestrator/boundary.md)
- [Action 模型](docs/orchestrator/action-model.md)
- [GUI / TUI 等价性](docs/orchestrator/gui-tui-parity.md)
- [Topology 模型](docs/orchestrator/topology-model.md)
- [Operation 模型](docs/orchestrator/operation-model.md)
- [Orchestrator 数据库](docs/orchestrator/database.md)
- [部署清单](docs/ops/deployment-checklist.md)
- [运维手册](docs/ops/ops-runbook.md)
- [可核对证据](docs/release/evidence.md)

历史/废弃文档已从仓库删除，其架构结论已并入[项目完成度总结](docs/completeness-summary.md)。
