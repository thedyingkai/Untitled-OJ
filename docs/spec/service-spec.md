# Service 规范

Service 是 OJOS Orchestrator 可导入、可校验、可计划、可启停、可连接、可观测的最小功能单元。题库、提交、比赛、用户、权限、公告、训练等 OJ 能力都属于具体 Service 自己的职责；Orchestrator 只负责编排这些 Service，并维护 Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport。

## service.yaml

`service.yaml` 是唯一正式 Service 契约。核心字段如下：

```yaml
schema_version: 1
id:
name:
version:
kind:
description:

runtime:
  mode:
  driver:
  root_allowed:
  non_root_allowed:
  start_policy:
  restart_policy:

endpoint:
  protocol:
  default_port:
  health_path:
  expose:
  routes:

requires:
  services:
  links:
  storage:
  database:
  queue:
  secrets:

provides:
  capabilities:
  endpoints:
  routes:
  workers:
  storage_buckets:
  events:

config_schema:

resources:
  cpu:
  memory:
  disk:
  gpu:
  network:

security:
  allow_privileged:
  allow_host_mount:
  allow_arbitrary_command:
  required_secrets:
  sandbox:
  network_policy:

source:
  type:
  ref:
  build:
  artifact:

ui:
  enabled:
  routes:
  menus:
  permissions:

permissions:

health:
  checks:
  timeout_seconds:
  interval_seconds:
```

`source` 只是 `service.yaml` 的来源描述字段，不是 Orchestrator 核心对象。构建产物、发布包和仓库来源也不进入当前核心对象集合。

## Service 类型

正式 `kind` 取值：

```text
frontend
backend-api
backend-worker
gateway
database
cache
storage
external
agent
```

`kind` 用于编排视图、默认计划和诊断分组，不把 Service 拆成新的核心对象。

## 安全约束

`service.yaml` 不允许任意 `command`、脚本、hook、`privileged`、`cap_add`、host mount 或明文 secret。secret 只能通过 `requires.secrets`、`security.required_secrets` 或 Link 的 `secret_ref` 表达。

Endpoint 只能声明 `default_port`；运行时实际 `IP:Port` 由 Orchestrator 绑定。Link 只能声明连接需求；真实 Link 由 Orchestrator 根据 Endpoint 创建。

## 基础 Service

当前基础 Service 为：

```text
gateway
web-shell
auth
problem-api
judge-api
judge-worker
postgres
redis
storage
```

每个 Service 必须提供 `service.yaml`，并通过 `orchestrator/core` 的契约校验。
