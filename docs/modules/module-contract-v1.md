# 模块契约 v1

Module Contract v1 是 OJOS 模块的稳定兼容起点。模块只要保持在该契约内，就可以通过 manifest/package/runtime 贡献 metadata、权限、菜单、前端路由元数据、Gateway route 元数据、服务、Worker、健康检查、存储元数据、事件、admin panel 元数据和拓扑，而不需要修改 Kernel、Gateway 或 Web Shell 主逻辑。

## 兼容策略

- `schema_version: 1` 是当前兼容起点。
- unknown top-level fields 默认拒绝。
- unknown `provides` fields 在 v1 中拒绝；新增 extension point 需要先设计和评审。
- dangerous unknown fields 在 manifest 任意位置拒绝。
- v1 新增字段必须向后兼容，不能改变旧 manifest 语义。
- 删除或重命名字段必须进入 `schema_version: 2`。
- 模块不能通过 manifest 执行代码、拉取远程代码或控制 host runtime。

危险字段名包括：

```text
secret
token
password
private_key
env
command
script
hook
image
mount
host_path
privileged
cap_add
postinstall
preinstall
remote_url
download_url
target_url
```

## 身份字段

- `schema_version`：必须为 `1`。
- `id`：小写模块 ID，例如 `ojos.sample-hello`。
- `name`：显示名。
- `version`：semver。
- `set`：模块集合。
- `kind`：`kernel`、`platform`、`feature`、`integration` 或 `metadata`。
- `status`：`builtin`、`external` 或 `demo`。
- `description`：描述，最长 2000 字符。

## 兼容与依赖

`compatibility.platform` 和 `compatibility.installer` 声明最低平台/安装器约束。`requires.modules` 声明模块依赖和版本约束。

## Provides

- `provides.permissions`：权限 key。
- `provides.roles`：角色元数据。
- `provides.components`：通用组件元数据。
- `provides.services`：服务生命周期元数据。
- `provides.workers`：Worker 生命周期元数据。
- `provides.frontend_routes`：前端路由元数据；Web Shell 不动态 import 未知组件。
- `provides.menus`：菜单元数据；disabled menu 不会成为 active navigation。
- `provides.gateway_routes`：route prefix 与 `service_id`；不能声明 arbitrary `target_url`。
- `provides.storage_buckets`：存储桶元数据。
- `provides.health_checks`：健康检查元数据。
- `provides.migrations`：模块拥有的迁移元数据。
- `provides.events`：publish/subscribe 元数据。
- `provides.scheduled_jobs`：计划任务元数据。
- `provides.admin_panels`：管理面板元数据。
- `provides.topology.nodes` / `provides.topology.edges`：拓扑贡献。

## Package

`.ojosmod` v1 package 包含：

```text
module.yaml
checksums.sha256
package.yaml
```

Package v1 只验证 checksum integrity。Signature trust policy 留到后续版本。

## Hotplug Level

Schema v1 支持 L0 metadata hotplug、L1 Gateway route table contribution、L2 controlled service plan metadata。不支持 dynamic frontend bundle、hook、remote module market 或完整模块热插拔自动化。

## 版本冻结

`schema_version: 1` 已冻结为当前兼容起点。破坏性变更必须使用 `schema_version: 2`；v1 的 additive fields 必须保持向后兼容。
