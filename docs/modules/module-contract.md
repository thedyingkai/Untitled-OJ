# Module Contract

> 文档状态：当前实现
> 最后更新：2026-06-27

## Contract Boundary

模块与 OJOS 主系统只通过以下 contract 交互：

- `module.yaml` manifest schema。
- `.ojosmod` package format。
- PostgreSQL module registry tables。
- Gateway Admin API。
- Frontend route/menu metadata。
- Operation history and audit log。

Installer core 不依赖 Go 代码，不依赖 frontend 代码。Gateway 也不直接读写本地模块文件，而是调用 internal Rust service。

## Declared Capabilities

一个模块可以声明：

- permissions
- components
- frontend_routes
- menus
- gateway_routes
- storage buckets
- health_checks
- migrations
- dependencies

未声明的能力不得由 installer 隐式公开。

## Security Contract

- 模块不能携带生产 secret。
- 模块不能绕过 Gateway、JWT、system.admin 权限和 internal auth。
- 模块包不能包含 `.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target`。
- v0 不执行模块中的脚本或 hook。
- v0 不支持远程模块市场。

## Repository Boundary

当前 installer 放在 monorepo 内，但按可拆仓标准实现。拆仓触发条件见 ADR：

```text
docs/architecture/adr/ADR-module-installer-repository-boundary.md
```

## Project Structure v2 Contract Addendum

Installer, Module Runtime, Module Registry and Topology are Kernel capabilities. Gateway is an App / edge adapter, and Web Shell is an App / frontend shell. Judge Core is a Feature Module that wraps the current problem-api, judge-api and judge-worker compatibility paths.

`schema_version: 1` now accepts the future-facing extension surface: permissions, roles, components, services, workers, frontend_routes, menus, gateway_routes, storage_buckets, health_checks, migrations, events, scheduled_jobs, admin_panels and topology nodes/edges.

Future modules should be added through `module.yaml`, `.ojosmod` package metadata and extension points. They must not require Kernel code changes, Gateway hardcoded navigation changes, Web Shell hardcoded menu changes, topology hardcoding, or changes to existing modules.

## Runtime Wiring v1 Addendum

`schema_version: 1` module contracts are now consumed by Runtime Snapshot. The following extension points can surface without page-specific hardcoding:

- `permissions` -> active permission registry
- `menus` -> Web Shell menu contribution candidates
- `frontend_routes` -> contribution viewer and future route registry
- `gateway_routes` -> runtime route table and conflict validation
- `services` / `workers` / `components` -> runtime components and topology nodes
- `storage_buckets` -> runtime storage metadata
- `health_checks` -> admin health metadata and snapshot health checks
- `events`, `scheduled_jobs`, `admin_panels` -> manifest-derived runtime operation metadata
- `topology.nodes` / `topology.edges` -> runtime topology graph

Active Runtime Snapshot only includes ENABLED modules. Disabled modules remain visible through registry detail and `include_disabled=true` admin inspection. Disabled metadata must not create clickable fake business entries or live gateway proxy routes.

Frontend contribution hotplug remains metadata-only. Dynamic JS, remote bundles, hooks and remote marketplace installation are still out of scope.

## Hotplug L1 Gateway Route Contract

`gateway_routes` uses `service_id` to reference a Gateway-trusted service. Manifests must not provide `target_url`, public URLs, localhost targets, Docker socket paths, raw ports, credentials or proxy commands.

Example:

```yaml
gateway_routes:
  - prefix: /api/problem
    service_id: problem-api
    auth_mode: user
    enabled: true
```

Gateway resolves `service_id` through its trusted service map. Runtime Route Table blocks unknown services, duplicate prefixes and reserved prefix claims. Reserved prefixes include `/api/auth`, `/api/admin/modules`, `/api/admin/health`, `/api/health`, `/api/internal` and `/api/judge/worker`.

Web Shell contribution hotplug stays safe: known compiled routes render their existing components; unknown `component_key` values are shown through the Module Contributions registry page. Web Shell does not dynamically import remote JavaScript, does not execute module bundles and does not load untrusted frontend code.

## Hotplug L2 Service / Worker Contract

`schema_version: 1` supports structured service and worker declarations for L2 foundation.

```yaml
provides:
  services:
    - id: problem-api
      name: Problem API
      kind: http
      lifecycle: managed
      trusted_runtime: compose
      compose_service: problem-api
      health_check_id: problem-api-health
      routes:
        - /api/problem
      required: true

  workers:
    - id: judge-worker
      name: Judge Worker
      kind: worker
      lifecycle: managed
      trusted_runtime: compose
      compose_service: judge-worker
      health_check_id: worker-cluster-health
      required: false
```

Allowed lifecycle values are `managed`, `metadata`, `external` and `manual`. L2 foundation only generates compose plans for trusted allowlisted `managed` services. `metadata` services enter snapshot/topology but cannot start, stop or restart.

Manifests may not specify executable runtime control fields such as `command`, `script`, `image`, `mount`, `host_path`, `privileged` or `cap_add`. A module may declare the trusted compose service identity, but it cannot declare arbitrary images, URLs, host mounts or shell commands.

## Hotplug L2 Controlled Apply Contract

The manifest describes service intent; it does not grant execution rights. Runtime apply is derived from trusted Kernel/operator policy, not from executable manifest fields.

Valid service declarations may reference `id`, `kind`, `lifecycle`, `trusted_runtime`, `compose_service`, `health_check_id`, `routes`, and `required`.

Invalid executable control remains forbidden:

- arbitrary `image`
- `command`
- `script`
- hook names used as executable lifecycle steps
- host `mount` or `host_path`
- `privileged`
- `cap_add`
- arbitrary `target_url`

Controlled apply uses a runtime plan with `commands[].argv`, an allowlisted `compose_service`, TTL, `operation_id`, `requires_confirmation`, and `allowed_targets`. A future module cannot make itself executable by adding command-like fields to `module.yaml`.
