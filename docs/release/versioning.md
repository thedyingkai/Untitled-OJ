# Versioning And Contract Freeze

日期：2026-06-28

## 冻结版本

| 契约 | 当前版本 | 兼容规则 |
| --- | --- | --- |
| Module manifest schema | `schema_version: 1` | 破坏性 manifest 变更必须进入 `schema_version: 2` |
| Runtime Snapshot | `version: 1` | 破坏性响应结构变更必须进入 snapshot version `2` |
| `.ojosmod` package format | `package.version: 1` | 破坏性 package layout 变更必须进入 package format `2` |
| OJOS release | `v0.1.0` | 当前题库、评测、Kernel、Runtime、Installer、SDK 发布基线 |

## Manifest 兼容性

所有仓库内 manifest 必须通过 schema v1：

- `modules/judge-core/module.yaml`
- `modules/demo-module/module.yaml`
- `modules/sample-hello/module.yaml`

Schema v1 拒绝 unknown top-level fields 和 dangerous fields。新增字段必须向后兼容；删除或重命名字段属于破坏性变更。

## Runtime Snapshot 兼容性

Runtime Snapshot v1 是当前模块贡献事实源，包含模块、权限、菜单、前端路由元数据、Gateway route、服务、Worker、健康、组件和拓扑。

`include_disabled=true` 只用于管理员检查 disabled registry contribution，不应作为 public runtime surface。

## Package 兼容性

Package format v1 是 zip-based `.ojosmod`，必须包含：

```text
module.yaml
checksums.sha256
package.yaml
```

`signature`、`signing_key_id`、`trusted_publisher` 在 v1 可为 `null`。Checksum integrity 不能证明发布者可信；remote module market 在 signature/trust policy 完成前仍不支持。

## Installer 入口版本

v0.1.0 的官方安装器入口：

- `ojosctl`：脚本、CI、服务器部署和受控 runtime apply。
- `ojos-installer-tui`：原生终端可视化管理界面。

Web Shell 只作为管理和运行状态视图，不作为官方安装器主入口。
