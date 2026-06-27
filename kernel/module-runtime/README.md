# Kernel Module Runtime

This directory is the canonical Project Structure v2 boundary for OJOS Kernel Module Runtime.

Phase 1 keeps the executable Go implementation under `services/gateway/internal/kernel/moduleruntime` as a compatibility adapter, because Gateway is still the public admin API process and moving every Go service in one step would create unnecessary runtime risk. The runtime contract is owned here:

- read module registry tables
- compute enabled modules
- export runtime snapshot
- aggregate permissions, menus, frontend routes, gateway routes, components, services, workers, health checks, and topology
- provide future runtime driver interfaces for controlled service and worker start/stop plans

The canonical API surface is:

```text
GET /api/admin/modules/runtime-snapshot
GET /api/admin/modules/topology
```

Current hotplug level is L0 metadata hotplug with partial L1 route-snapshot groundwork. L2 service hotplug and L3 frontend contribution hotplug are design targets only; OJOS v0 does not execute untrusted scripts or dynamic frontend bundles.
