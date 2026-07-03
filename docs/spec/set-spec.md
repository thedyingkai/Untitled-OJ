# 部署模板规范

部署模板（Deployment Template）是只读的本地部署辅助材料。它不是运行时对象、数据库表、正式 action 层或
业务 API。它只描述推荐服务、默认 endpoint、默认 link、安装顺序、启动顺序和放置策略。

## template.yaml

```yaml
schema_version: 1
id:
name:
description:

scenario:
  type:
  recommended_for:

services:
  - id:
    required:
    count:
    placement:
    config:

default_endpoints:
  - service:
    port:
    protocol:
    expose:

default_links:
  - from:
    to:
    protocol:
    auth_mode:
    scope:
    required:

policies:
  placement:
  security:
  network:
  health:

operations:
  install_order:
  start_order:
  stop_order:

notes:
```

## 当前本地模板

仓库保留五个本地模板：

```text
single-node-oj
distributed-oj
judge-worker-node
course-judge
service-development
```

部署模板不引入 host 对象、device 对象、install instance 对象、package 对象或 service-set 持久化。放置由
`placement` 策略和运行时 Endpoint 身份 `ip:port:service-name` 表达。

每个被引用的 Service 都必须存在于 `services/*/service.yaml` 下。`default_endpoints` 和 `default_links` 只能
引用同一本地模板中列出的服务。如果某个必需的 link 目标由另一个模板或外部 endpoint 提供，模板必须在
`policies.network.required_external_links` 中声明。

运行时，Orchestrator 从具体的 Endpoint 创建真实的 Link。`service-name[*]` 值始终通过查询同名运行中 Endpoint
派生，不从模板加载，也不持久化。
