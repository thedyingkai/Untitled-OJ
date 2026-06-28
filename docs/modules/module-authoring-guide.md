# Module Authoring Guide

模块作者应从 `ojosctl module init` 开始，再在 `schema_version: 1` 范围内编辑 `module.yaml`。

## 权限

在 `provides.permissions` 中声明 permission key。模块启用后，Runtime Snapshot 会把这些权限暴露给管理员权限注册表。

## 菜单与前端路由元数据

在 `provides.menus` 声明菜单元数据，在 `provides.frontend_routes` 声明前端路由元数据。未知 `component_key` 会进入安全的模块贡献视图；Web Shell 不会动态 import JavaScript。

## Gateway Route

声明 prefix 和 `service_id`：

```yaml
gateway_routes:
  - prefix: /api/sample-hello
    service_id: sample-hello-api
    auth_mode: user
    enabled: false
```

Manifest 不能提供 URL。Gateway 负责 trusted upstream configuration 和 reserved prefix protection。

## Services 与 Workers

Metadata-only declaration 是默认安全选择：

```yaml
services:
  - id: sample-hello-metadata-service
    lifecycle: metadata
    trusted_runtime: metadata
```

Managed compose service 必须由部署/operator allowlist 批准。模块不能通过声明 image、command 或 mount 让自己变成可执行服务。

## Health 与 Topology

在 `provides.health_checks` 中声明 health metadata，在 `provides.topology` 中声明 nodes/edges。模块启用后，Runtime Snapshot 和 Admin Topology 会自动展示这些贡献。

## 生命周期

Install 写入 registry metadata。Enable 激活 runtime contribution。Disable 保留 registry data，但从 active snapshot 移除 active contribution。`include_disabled=true` 只供管理员检查。
