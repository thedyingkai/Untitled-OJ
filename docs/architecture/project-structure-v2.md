# Project Structure v2

> 文档状态：当前实现，Phase 1 compatibility
> 适用范围：架构 / 开发 / 部署 / 模块作者
> 最后更新：2026-06-27

Project Structure v2 将 OJOS 分为 Kernel、Apps、Modules、Tools、Deploy、Docs 六层。目标是让未来模块只通过 manifest、package 和 extension points 接入，而不是修改 Kernel、Gateway、Web Shell 或已有模块。

## 目标结构

```text
kernel/
  contracts/
  installer/
    core/
    service/
    cli/
  module-runtime/
  module-registry/
  topology/
  policy/
  audit/
  config/
  health/

apps/
  gateway/
  web-shell/

modules/
  judge-core/
  demo-module/

tools/
deploy/
docs/
```

## Phase 1 实施范围

已进入 Phase 1：

- Rust Installer source 移动到 `kernel/installer/core`、`kernel/installer/service`、`kernel/installer/cli`。
- 新增 Kernel Module Runtime skeleton，提供 runtime snapshot 类型、聚合逻辑和 API。
- Judge Core 仍以 `modules/judge-core/module.yaml` 包装现有服务。
- Gateway、Go services 和 Vue frontend 保留原路径作为 legacy compatibility，避免一次性物理移动导致运行风险。

## 边界

- Kernel：installer、registry、runtime、topology、policy、audit、config、health。
- Apps：Gateway 是 edge adapter；Web Shell 是 frontend shell。
- Modules：Judge Core、Demo Module 和未来 Contest/Training/Remote OJ 等 feature module。
- Tools：`ojosctl`、验证脚本、运维工具。
- Deploy：compose、migrations、package deployment。
- Docs：正式文档和 ADR。

## Legacy Compatibility

以下路径在 Phase 1 仍保留：

```text
services/gateway
services/auth
services/problem-api
services/judge-api
services/judge-worker
frontend
```

这些路径分别对应未来 `apps/gateway`、platform identity、`modules/judge-core/services/*`、`modules/judge-core/workers/*` 和 `apps/web-shell`。

## 添加未来模块

未来模块原则上只需要：

```text
modules/<module-id>/module.yaml
module services/workers/frontend contribution
.ojosmod package
installer install/enable
```

不得要求修改 Kernel 代码、Gateway 硬编码、Web Shell 主导航硬编码、权限硬编码、topology 硬编码或已有模块代码。

## Kernel Runtime Wiring v1

Project Structure v2 now has an operational wiring layer: Gateway and Web Shell read Runtime Snapshot instead of treating module metadata as page-local hardcoding.

Implemented in this phase:

- Runtime Snapshot v1 includes version, generated_at, active module contributions, route table inputs, manifest-derived metadata and topology.
- Admin Runtime Routes API exposes registry-driven gateway route table and conflict validation.
- Web Shell renders module-provided menu entries from Runtime Snapshot when admin access is available, while retaining static compatibility routes for existing Judge Core pages.
- Admin Topology renders Runtime Snapshot topology nodes/edges and keeps module graph compatibility fields.
- Permission admin page reads active module permission registry from Runtime Snapshot.

Adding a metadata-only module should require only manifest/package installation and enablement. Service proxy cutover, runtime service driver and frontend bundle loading remain future phases.

## Hotplug L1 Completion Addendum

Project Structure v2 now treats Gateway dynamic proxy and Web Shell contribution registry as Kernel Runtime consumers. Future ordinary modules should be able to add enabled permissions, menus, frontend metadata, topology, health metadata and trusted gateway routes through manifest/package/installer/runtime snapshot without editing Kernel code.

This does not implement L2 service runtime driver, L3 dynamic frontend bundle loading or L4 full module automation.