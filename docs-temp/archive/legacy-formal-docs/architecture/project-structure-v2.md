# 项目结构 v2

> 文档状态：当前实现，包含兼容路径
> 适用范围：架构 / 开发 / 部署 / 模块作者
> 最后更新：2026-06-28

项目结构 v2 将 OJOS 分为 Kernel、Apps、Modules、Tools、Deploy、Docs 六层。目标是让未来普通模块只通过 manifest、package 和 extension points 接入，而不是修改 Kernel、Gateway、Web Shell 或已有模块。

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

## 当前落地范围

当前已经落地：

- Rust Installer source 位于 `kernel/installer/core`、`kernel/installer/service`、`kernel/installer/cli` 和 `kernel/installer/tui`。
- Kernel Module Runtime 提供 runtime snapshot 类型、聚合逻辑和 API。
- Judge Core 仍以 `modules/judge-core/module.yaml` 包装现有服务。
- Gateway、Go services 和 Vue frontend 保留原路径作为兼容路径，避免一次性物理移动导致运行风险。

## 边界

- Kernel：installer、registry、runtime、topology、policy、audit、config、health。
- Apps：Gateway 是 edge adapter；Web Shell 是 frontend shell。
- Modules：Judge Core、Demo Module 和未来 Contest/Training/Remote OJ 等 feature module。
- Tools：`ojosctl`、验证脚本、运维工具。
- Deploy：compose、migrations、package deployment。
- Docs：正式文档和 ADR。

## 兼容路径

以下路径在 v0.1.0 仍保留：

```text
services/gateway
services/auth
services/problem-api
services/judge-api
services/judge-worker
frontend
```

这些路径分别对应未来可能拆分的 `apps/gateway`、platform identity、`modules/judge-core/services/*`、`modules/judge-core/workers/*` 和 `apps/web-shell`。当前发布不执行物理拆分。

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

项目结构 v2 已具备运行时接线层：Gateway 和 Web Shell 读取 Runtime Snapshot，不把模块元数据作为页面级硬编码处理。

当前已实现：

- Runtime Snapshot v1 包含 version、generated_at、active module contributions、route table inputs、manifest-derived metadata 和 topology。
- Admin Runtime Routes API 暴露 registry-driven gateway route table 和 conflict validation。
- Web Shell 在具备 admin access 时从 Runtime Snapshot 渲染模块提供的 menu entries，同时保留现有 Judge Core 页面的 static compatibility routes。
- Admin Topology 渲染 Runtime Snapshot topology nodes/edges，并保留 module graph compatibility fields。
- Permission admin page 从 Runtime Snapshot 读取 active module permission registry。

新增 metadata-only 模块原则上只需要 manifest/package installation 和 enablement。Service proxy cutover、完整 runtime service driver 和 frontend bundle loading 仍是后续能力。

## Hotplug L1 状态

项目结构 v2 将 Gateway dynamic proxy 和 Web Shell contribution registry 视为 Kernel Runtime consumers。普通模块应能通过 manifest/package/installer/runtime snapshot 增加 enabled permissions、menus、frontend metadata、topology、health metadata 和 trusted gateway routes，而不修改 Kernel code。

这不代表 L3 dynamic frontend bundle loading 或 L4 full module automation 已完成。

## Hotplug L2 Foundation

项目结构 v2 在 Kernel Runtime 边界下包含 service runtime foundation。兼容实现仍位于 Gateway 的 `services/gateway/internal/kernel/moduleruntime`，但接口形态按 Kernel 设计，避免业务特定 runtime control。

当前边界：

- Module manifests 声明 services/workers 和 trusted runtime metadata。
- Kernel Runtime 生成 service state、health、topology 和 lifecycle plans。
- Gateway 和 Web Shell 消费 runtime plans/status，但不控制 Docker 或 hosts。
- `ojosctl runtime ...` 提供本地 plan/status inspection。

Runtime APIs 稳定后，未来 repo splitting 应保持 runtime driver contracts 与 Gateway edge code 分离。

## Hotplug L2 Controlled Apply

项目结构 v2 不让 Apps 持有 apply authority。Gateway 和 Web Shell 是 Apps/adapters：它们暴露 plan、status 和 operation history，但不拥有 host lifecycle execution。

Controlled apply path 属于 Tools / Operator：

```text
tools or kernel/installer/cli -> ojosctl runtime apply-plan
future operator -> controlled runtime apply
```

当前 monorepo placement 将 `ojosctl` 保留在 `kernel/installer/cli`，runtime driver 兼容代码保留在 `services/gateway/internal/kernel/moduleruntime`。目标拆分边界仍为：

- Kernel Runtime 定义 plan/state/driver contracts。
- Apps 消费 runtime APIs，但不拥有 host control。
- Tools/Operator apply trusted local plans。
- Modules 只通过 manifest metadata 声明 services/workers。

v0.1.0 不改变 repo split 决策，也不实现 remote module service deployment。
