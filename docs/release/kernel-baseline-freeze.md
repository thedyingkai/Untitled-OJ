# Kernel Baseline Freeze

日期：2026-06-28

本文冻结 OJOS v0.1.0 发布基线中的 Kernel、Installer、Runtime、Module SDK 和 Judge Core 当前能力。本文只描述已经实现并可验收的能力，不代表 Contest 已实现，也不代表 full hotplug 完成。

## 已完成能力

- Installer Core：`module.yaml` 校验、依赖计划、package checksum、install/enable/disable 计划。
- Module Registry：Kernel、Platform、Judge Core、demo module 和 sample module 的注册表视图。
- Module Runtime Snapshot：`version: 1`，聚合已启用模块的权限、菜单、路由、服务、Worker、健康和拓扑贡献。
- Topology from Snapshot：模块、依赖、服务、Worker、Gateway route、health、menu 和 manifest 拓扑节点从 Runtime Snapshot 派生。
- Dynamic Gateway Route Table：Gateway 从 Runtime Snapshot 构建可信动态路由表，并支持 reload。
- Dynamic Gateway Proxy：已启用、可信 `service_id` 路由可在 Gateway 中代理，仍受 auth mode 和 reserved prefix 保护。
- Web Shell Contribution Registry：Web Shell 从 Runtime Snapshot 展示菜单、前端路由元数据、模块贡献和拓扑，不动态执行模块 JS。
- Permission Registry：模块权限进入权限注册表，并随模块 enable/disable 影响 active snapshot。
- Health Aggregation：管理健康页聚合核心服务和 Judge 状态。
- Service Runtime Driver Foundation：服务/Worker 声明、状态、健康、生命周期和计划数据已经进入 Runtime。
- Controlled Apply：`ojosctl runtime apply-plan` 支持 trusted compose allowlist、dry-run、confirm、TTL、锁、超时、输出裁剪和 operation history。
- Module SDK：Module Contract v1、`ojosctl module init`、package/verify、authoring/testing 文档和 compatibility harness。
- Sample Module Compatibility Harness：`modules/sample-hello` 证明普通 metadata 模块可通过 manifest/package/runtime 接入。
- Judge Core Moduleized Boundary：Judge Core 作为 feature module 出现在 snapshot、routes、services 和 topology 中，disable/uninstall 仍受保护。
- Native Installer Entry：正式原生入口为 `ojosctl` 和 `ojos-installer-tui`；Web Shell 只作为管理视图。

## 未完成能力

- L3 dynamic frontend bundle。
- 任意模块服务镜像部署和完整 service runtime driver。
- remote module market。
- package signature / publisher trust policy。
- hook execution。
- full hotplug automation。
- true multi-machine runtime apply。
- Judge Core GA。
- Contest 或其他真实业务模块。

## Hotplug 等级

| 等级 | 当前状态 | 已实现内容 | 边界 |
| --- | --- | --- | --- |
| L0 Metadata Hotplug | 完成 | registry、snapshot、permission、menu、topology、health metadata | 仅 metadata |
| L1 Route/Menu/Topology/Permission Hotplug | 基本完成 | dynamic route table、trusted proxy、Web Shell contribution registry、安全 fallback | 不加载动态前端 JS |
| L2 Service Runtime Foundation + Controlled Apply | foundation 完成 | service/worker 声明、health/state、plan、`ojosctl` controlled apply | Gateway/Web 不 apply |
| L3 Dynamic Frontend Extension | 未完成 | 只展示 metadata | 需要签名、CSP、sandbox 等安全设计 |
| L4 Full Module Hotplug | 未完成 | 无完整自动化 | 需要 market/trust/runtime/operator 设计 |

## 统一验收入口

默认验收不执行危险 apply：

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
```

Controlled Apply 必须显式开启：

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -RunControlledApply -SkipDockerBuild
```

## 发布结论条件

只有以下检查全部通过，v0.1.0 才能作为发布基线：

- `acceptance-kernel` 通过。
- `verify-static` 通过。
- `e2e-api` 与 `e2e-module-compat` 均为 `failed=0`。
- Go、Rust、Judge Worker 和 Frontend 构建/测试通过。
- `path_leaks=0`。
- 普通用户访问管理端点返回 `403`，无 token 返回 `401`。
- `module-installer` healthy，Gateway running。

本冻结文档不允许直接开始完整 Contest；真实业务模块仍必须先经过 feature planning 和 skeleton 范围门禁。
