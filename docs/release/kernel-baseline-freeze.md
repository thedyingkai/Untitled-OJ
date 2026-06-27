# Kernel Baseline Freeze

Date: 2026-06-27

This document freezes the current Kernel / Installer / Runtime / SDK baseline before the first real feature module after Judge Core.

## Completed Baseline Capabilities

- Installer Core: manifest validation, dependency planning, package checksums, local install/enable/disable metadata lifecycle.
- Module Registry: built-in kernel/platform modules, Judge Core, demo module and SDK sample module registry surfaces.
- Module Runtime Snapshot: version `1` snapshot aggregates active module contributions and supports admin `include_disabled=true`.
- Topology from snapshot: module, dependency, service, worker, route, health, menu and manifest topology nodes/edges are generated from runtime data.
- Dynamic Gateway route table: enabled module routes are built from registry/runtime snapshot and can be reloaded.
- Dynamic Gateway proxy: enabled trusted routes can proxy through `service_id` to trusted upstreams with auth enforcement.
- Web Shell contribution registry: menus, frontend route metadata, admin panels and contribution views are sourced from runtime snapshot.
- Permission registry: module permissions enter runtime/admin views and are filtered by module enablement.
- Health aggregation: admin health includes core service and Judge status surfaces.
- Service Runtime Driver foundation: services/workers have state, health, lifecycle and plan metadata.
- Controlled Apply via `ojosctl` / operator: trusted compose allowlist plans can be applied locally with confirm, dry-run, timeout, lock and operation history.
- Module SDK: contract v1 docs, scaffold command, package/verify commands and authoring/testing guides exist.
- Sample module compatibility harness: `modules/sample-hello` proves ordinary metadata modules can install/enable/disable without sample-specific Gateway/Web Shell core changes.
- Judge Core moduleized boundary: Judge Core is a feature module exposed through snapshot, routes, services and topology, while disable/uninstall protection remains in place.

## Not Completed

- L3 dynamic frontend bundle.
- Full service runtime driver for arbitrary module services.
- Remote module market.
- Package signature / publisher trust policy.
- Hook execution.
- Full hotplug automation.
- True multi-machine runtime apply.
- Judge Core GA.

## Hotplug Level Freeze

| Level | Current status | Implemented | Boundary |
| --- | --- | --- | --- |
| L0 Metadata Hotplug | Complete | Registry, snapshot, permissions, menus, topology metadata, health metadata | Metadata only |
| L1 Route/Menu/Topology/Permission Hotplug | Basically complete | Dynamic route table, trusted dynamic proxy, Web Shell contribution registry, safe unknown component fallback | No dynamic frontend JS |
| L2 Service Runtime Foundation + Controlled Apply | Foundation complete | Service/worker declarations, health/state, route-health linkage, plan generation, controlled `ojosctl` apply | Gateway/Web do not apply |
| L3 Dynamic Frontend Extension | Not complete | Metadata placeholder only | Needs signed/sandboxed frontend design |
| L4 Full Module Hotplug | Not complete | No full automation | Needs market/trust/runtime/operator design |

## Baseline Acceptance Entry

The local baseline acceptance entry is:

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
```

Controlled apply remains opt-in:

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -RunControlledApply
```

The default acceptance path must not apply runtime plans.

## Release Decision

The baseline is ready for a first real feature module only after `acceptance-kernel`, `verify-static`, `e2e-api`, `e2e-module-compat`, Go tests, Rust tests and frontend build are all green with `path_leaks=0`, ordinary user `403` and no-token `401`.

This freeze does not permit Contest implementation by itself; it only opens the feature gate once the acceptance matrix is green.
