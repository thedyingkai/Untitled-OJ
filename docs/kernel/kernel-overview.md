# Kernel Overview

> 文档状态：当前实现，Phase 1 skeleton
> 适用范围：Kernel 开发 / 模块开发 / 架构评审
> 最后更新：2026-06-27

OJOS Kernel 负责模块系统的系统级能力。它不包含题目、比赛、训练、讨论或远程 OJ 的业务逻辑。

## Kernel 能力

```text
Module Installer
Module Registry
Module Runtime
Module Lifecycle
Module Topology
Module Health
Module Policy
Module Audit
Module Config
Module Package Verification
Module Dependency Resolver
Module Operation Lock
Module Operation History
```

## Kernel Built-ins

```text
ojos.kernel.installer
ojos.kernel.module-runtime
ojos.kernel.module-registry
ojos.kernel.topology
ojos.kernel.policy
ojos.kernel.audit
ojos.kernel.config
ojos.kernel.health
```

Kernel built-ins 不允许 disable 或 uninstall。

## Platform Built-ins

```text
ojos.platform.gateway
ojos.platform.web-shell
ojos.platform.identity-access
ojos.platform.storage
ojos.platform.observability
```

Platform built-ins 默认受保护，不作为普通 feature module 卸载。

## Feature Modules

```text
ojos.judge-core
ojos.demo-module
future ojos.contest
future ojos.training
future ojos.group
future ojos.remote-oj
```

Feature modules 根据 dependency、dependent 和 safety policy 判断 enable、disable、upgrade、rollback 和 uninstall。

## Kernel Runtime Wiring v1

The Kernel now treats Runtime Snapshot as the operational entry point for module-provided permissions, menus, frontend route metadata, gateway route metadata, health checks, topology and component surfaces. Installer, Registry, Runtime and Topology remain Kernel capabilities. Gateway and Web Shell are adapters that read the Kernel runtime surface.

Future modules should appear in Module Center, runtime snapshot, contribution viewer, topology and permission registry through manifest/package installation. They should not require Kernel code changes for metadata-level L0 hotplug.

Boundaries retained:

- No B Contest work is implemented here.
- No remote module marketplace is implemented.
- No hook execution is allowed.
- No dynamic untrusted frontend bundle is loaded.
- A/Judge Core remains protected and is not marked GA.

## Hotplug L1 Completion

Kernel Runtime now provides dynamic gateway route activation for enabled module routes. Gateway remains the edge adapter and resolves module `service_id` values through a trusted service map. Web Shell remains the frontend shell and renders unknown module components through safe contribution metadata pages.

L2 service/worker runtime driver, L3 dynamic frontend bundles and L4 full module hotplug are not implemented.
## Hotplug L2 Foundation

Kernel Runtime now owns the service and worker lifecycle model for modules. Gateway remains an adapter: it can read runtime services, generate plans and make routing decisions from service health, but it does not control the host or Docker socket.

New Kernel runtime responsibilities in this foundation phase:

- Parse service and worker declarations from module manifests.
- Track service state and health in Runtime Snapshot.
- Generate plan-only start/stop/restart/reload/health responses.
- Bind gateway route status to service health.
- Add runtime service and worker nodes to topology.

Still out of scope: arbitrary service image deployment, remote module marketplace, hook execution, Docker socket control, Web-triggered apply, L3 dynamic frontend bundles and L4 full hotplug automation.

## Hotplug L2 Controlled Apply

Kernel Runtime now has a controlled apply boundary for runtime plans. The Kernel generates structured plans, while `ojosctl` or a future operator is the only component allowed to apply trusted local compose actions.

Boundary summary:

- Gateway and Web Shell remain control-plane viewers and plan generators.
- Gateway/Web do not mount Docker socket and do not execute Docker or shell commands.
- Module Installer does not become a host-control process.
- Manifests still cannot specify `command`, `script`, arbitrary `image`, host `mount`, `privileged`, or `cap_add`.
- Compose apply is limited to trusted allowlisted services and fixed compose configuration.
- Apply requires confirm, supports dry-run, uses locks/timeouts, and records operation history/audit.

This phase completes a controlled L2 apply path for known local services. It does not implement L3 dynamic frontend bundles, remote module market, hooks, arbitrary module service deployment, or full hotplug automation.
