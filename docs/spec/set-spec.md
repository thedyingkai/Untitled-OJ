# Set 规范

Set 是推荐部署组合，不是运行时对象，不提供业务 API，也不包含 OJ 业务逻辑。Set 只描述需要哪些 Service、默认 Endpoint、默认 Link、安装顺序、启动顺序和部署策略。

## set.yaml

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

Set 不使用额外主机对象、设备对象、安装实例对象或包对象。部署位置通过 `placement` 策略和 Endpoint 的 `IP:Port` 表达。

## 正式 Set

当前只保留五个正式 Set：

```text
single-node-oj
distributed-oj
judge-worker-node
course-judge
service-development
```

Set 引用的 Service 必须能在 `services/*/service.yaml` 中找到。`default_endpoints` 和 `default_links` 只能引用本 Set 内的 Service。跨 host 连接由 Orchestrator 在运行时根据 Endpoint 创建真实 Link。
