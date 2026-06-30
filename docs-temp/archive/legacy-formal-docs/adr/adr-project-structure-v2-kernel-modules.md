# ADR：项目结构 v2、Kernel 与 Modules

> 状态：已接受，按阶段落地
> 日期：2026-06-27

## 背景

OJOS 已具备 Rust Module Installer、Module Registry v0、模块拓扑视图、Judge Core module metadata 与 runtime validation。Installer、registry、lifecycle、runtime snapshot、topology、policy、audit 和 config 都是 Kernel 能力，不是普通业务模块，也不是普通业务服务。

如果未来 Contest、Training、Remote OJ、Discussion、Group、Print、Balloon 或 Clarification 等模块需要修改 Gateway 主逻辑、Web Shell 主导航、permission 硬编码、topology 硬编码或 installer 核心代码，模块系统就没有达到设计目标。

## 决策

OJOS 继续保留 monorepo，但引入明确架构层级：

```text
kernel/
  contracts/
  installer/
    core/
    service/
    cli/
    tui/
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

Installer Rust source 归入 `kernel/installer/*`。Gateway、Go services 和 Vue frontend 在 v0.1.0 保留当前物理路径，作为兼容路径，避免一次性物理迁移造成运行风险。

## 为什么 Installer 不是普通服务

Installer 拥有 package verification、manifest validation、dependency planning、lifecycle state、operation locks、operation history 和 audit。这些是系统级不变量。业务模块只能消费这些能力，不能拥有这些能力。

Installer service 可以作为 container 运行，但源码和职责归属 Kernel。Gateway 只是 adapter，通过 admin HTTP APIs 暴露 Kernel installer operations。

## 层级归属

Kernel 拥有：

- Module Installer。
- Module Registry。
- Module Runtime。
- Module Lifecycle。
- Module Topology。
- Module Health。
- Module Policy。
- Module Audit。
- Module Config。
- Module package verification。
- Dependency resolver。
- Operation lock 与 operation history。

Modules 拥有：

- Feature-specific services、workers、frontend contributions、migrations、permissions、health checks 和 topology declarations。
- Judge Core 是当前第一个核心 feature module，但不标记通用可用状态。

Apps 拥有：

- Gateway：public edge adapter。
- Web Shell：frontend shell、layout、route/menu renderer 和只读管理视图。

Deploy 拥有：

- Compose、migrations、runtime configuration、package deployment directories 和 environment templates。

Tooling 拥有：

- Developer/operator CLIs、TUI、release artifact 脚本和验证脚本。

## Monorepo 边界

OJOS 当前不拆分仓库，因为 module package schema、runtime APIs、installer release flow 和 module lifecycle contracts 仍在稳定。Monorepo 能在 v0/v1 期间保持 DB schema、Gateway adapter、Web Shell、compose 和 installer validation 同步。

未来拆分触发条件：

- Module package format 与 runtime APIs 稳定。
- Installer CLI/service 需要独立 release cadence。
- 外部 deployments 或项目复用 Installer。
- Module repository distribution 成为必要能力。
- 主仓库 CI 被独立 installer/module release tasks 明显拖慢。

## 扩展原则

未来模块必须通过 manifest/package contracts 和 extension points 接入：

```text
permissions
roles
menus
frontend_routes
gateway_routes
services
workers
migrations
storage_buckets
health_checks
events
scheduled_jobs
admin_panels
topology.nodes
topology.edges
```

v0 不执行 operation hooks，也不动态加载不可信 frontend bundle。

新增普通模块不应要求修改 Kernel logic、Gateway hard-coded routes、Web Shell navigation、permission hard-coding、topology hard-coding 或已有模块。

## Hotplug 等级

- L0 Metadata hotplug：dynamic registry、permissions、menus、topology、health 和 installation state。
- L1 Route/Menu/Topology/Permission hotplug：Gateway 与 Web Shell 读取 registry/snapshot，不硬编码模块贡献。
- L2 Service runtime foundation + controlled apply：runtime driver 生成计划，`ojosctl`/operator 受控 apply，v0 不暴露 Docker socket 给 Gateway/Web。
- L3 Dynamic frontend extension：未完成，不执行不可信 JavaScript。
- L4 Full module hotplug：未完成，package verification、service deployment、routes、permissions、frontend contributions、health 和 rollback 尚未全部自动化。

v0.1.0 目标是冻结 L0、L1 与 L2 foundation，不宣称完整模块热插拔自动化已经完成。

## 后果

- `kernel/installer/*` 是 canonical Rust installer source location。
- `kernel/installer/tui` 是官方原生 TUI source location。
- 顶层 `tools/ojosctl` wrapper 可在未来增加，但当前 CLI source 属于 Kernel Installer。
- `services/gateway/internal/moduleregistry` 是兼容路径，未来可向 `kernel/module-registry` 演进。
- `frontend` 是兼容路径，未来可向 `apps/web-shell` 演进。
- `services/problem-service`、`services/judge-api` 和 `services/judge-worker` 是 Judge Core 当前实现路径，未来可向 `modules/judge-core` 内部实现路径演进。
