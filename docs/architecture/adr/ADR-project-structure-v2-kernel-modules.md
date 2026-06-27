# ADR: Project Structure v2, Kernel and Modules

> Status: Accepted for phased implementation
> Date: 2026-06-27

## Context

OJOS now has a Rust Module Installer, Module Registry v0, topology pages, Judge Core module metadata, and runtime validation. Before Project Structure v2, the installer lived next to ordinary business services. That shape was misleading: installer, registry, lifecycle, runtime snapshot, topology, policy, audit and config are Kernel capabilities. They are not a feature module and not a normal business service.

If future modules such as Contest, Training, Remote OJ, Discussion, Group, Print, Balloon or Clarification require edits to Gateway code, frontend navigation, permission hard-coding, topology hard-coding or installer code, the module system has failed its purpose.

## Decision

OJOS will keep the monorepo for now, but Project Structure v2 will introduce explicit architectural layers:

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

Phase 1 physically moves the Rust installer source into `kernel/installer/*`, adds a Kernel Module Runtime skeleton, and documents legacy compatibility paths. Go Gateway, existing Go services and the Vue frontend stay in their current paths during Phase 1 to avoid a risky one-shot move.

## Why Installer Is Not A Normal Service

The installer owns package verification, manifest validation, dependency planning, lifecycle state, operation locks, operation history and audit. Those are system-level invariants. A business module consumes those invariants; it must not own them.

The installer service may still run as a container, but its source and ownership belong to Kernel. Gateway is only an adapter that exposes Kernel installer operations through admin HTTP APIs.

## Layer Ownership

Kernel owns:

- Module Installer.
- Module Registry.
- Module Runtime.
- Module Lifecycle.
- Module Topology.
- Module Health.
- Module Policy.
- Module Audit.
- Module Config.
- Module package verification.
- Dependency resolver.
- Operation lock and operation history.

Modules own:

- Feature-specific services, workers, frontend contributions, migrations, permissions, health checks and topology declarations.
- Judge Core is the first core feature module.

Apps own:

- Gateway as public edge adapter.
- Web Shell as frontend shell, layout and route/menu renderer.

Deploy owns:

- Compose, migrations, runtime configuration, package deployment directories and environment templates.

Tooling owns:

- Developer/operator CLIs and verification scripts.

## Monorepo Boundary

OJOS will not split repositories immediately because module package schema, runtime APIs, installer release flow and module lifecycle contracts are still stabilizing. A monorepo keeps DB schema, Gateway adapter, frontend shell, compose and installer validation aligned during v0/v1.

Future split triggers:

- Module package format and runtime APIs are stable.
- Installer CLI/service need an independent release cadence.
- External deployments or projects reuse the installer.
- Module repository distribution becomes necessary.
- Main repo CI is materially slowed by independent installer/module release tasks.

## Extension Principle

Future modules must integrate through manifest/package contracts and extension points:

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
operation_hooks declared but not executed in v0
```

Adding a module should not require edits to Kernel logic, Gateway hard-coded routes, frontend shell navigation, permission hard-coding or existing modules.

## Hotplug Levels

- L0 Metadata hotplug: dynamic registry, permissions, menus, topology, health and installation state without restart.
- L1 Gateway route hotplug: Gateway reads route registry/snapshot instead of hard-coded module routes.
- L2 Service hotplug: a runtime driver starts/stops module services/workers through a controlled operator plan. v0 does not expose Docker socket.
- L3 Frontend contribution hotplug: web shell renders registered menus/routes and generic module pages. Dynamic JS bundle execution is deferred.
- L4 Full module hotplug: package verification, service deployment, routes, permissions, frontend contributions, health and rollback are automated.

Current target: implement L0, partially implement L1 contracts, design L2, reserve safe L3 contracts, and do not execute untrusted dynamic JavaScript.

## Consequences

- `kernel/installer/*` becomes the canonical Rust installer source location.
- A top-level `tools/ojosctl` wrapper may be added later, but the current CLI source belongs to Kernel Installer.
- `services/gateway/internal/moduleregistry` remains a Phase 1 compatibility package and should move toward `kernel/module-registry`.
- `frontend` remains Phase 1 compatibility for `apps/web-shell`.
- `services/problem-api`, `services/judge-api` and `services/judge-worker` remain Phase 1 compatibility for `modules/judge-core` implementation paths.
