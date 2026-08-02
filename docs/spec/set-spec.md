# 部署模板规范

部署模板（Deployment Template）位于 `sets/*.yaml`。它是只读的部署参考和预览输入，不是数据库对象，也没有 `set.apply`、`set.expand` 这类正式 action。

模板描述 Service 组合、默认 Endpoint、默认 Link、放置策略和操作顺序。真正的 Endpoint 与 Link 仍由 Orchestrator 以运行时对象创建。

## 文件结构

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
    required_external_links:
  health:

operations:
  install_order:
  start_order:
  stop_order:

notes:
```

`services` 项也可以只写 Service ID 字符串。对象形式的默认值是 `required: true`、`count: 1`。

## 校验规则

- `schema_version` 当前只能是 `1`，模板至少包含一个 Service。
- 所有 Service ID 必须唯一，并对应 `services/*/service.yaml`。
- `default_endpoints` 和 `default_links` 只能引用本模板内的 Service。
- Service manifest 中的必需 Link 要么出现在 `default_links`，要么列入 `policies.network.required_external_links`。
- install/start/stop 顺序只能引用模板内 Service，不能重复。
- 模板不创建 host、device、installation、package 或持久化 service-set。

运行时身份仍是 `ip:port:service-name`。模板中的 `service-name[*]` 只是一种选择表达，最终目标从实际运行 Endpoint 推导，不保存为地址。

## 仓库内模板

```text
single-node-oj
distributed-oj
judge-worker-node
course-judge
service-development
```

`preview_deployment_template` 只返回模板 ID、Service 列表和默认 Link。想执行部署，必须把预览内容转换为正式 Service、Endpoint、Link 和 Operation 动作。
