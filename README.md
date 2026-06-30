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

OJ 业务服务使用独立业务数据库：

```text
OJ_DATABASE_URL
```

Orchestrator 不写 OJ 业务表；OJ 业务服务也不能直接写 Orchestrator 表。

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

正式文档位于 `docs/`：

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
- [可核对证据](docs/release/evidence.md)

历史文档位于 `docs-temp/`，不作为当前正式架构依据。
