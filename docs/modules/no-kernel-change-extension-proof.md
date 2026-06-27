# No-Kernel-Change Extension Proof

Sample Hello demonstrates ordinary module extension without changing Kernel, Gateway or Web Shell core logic.

## Files Added For The Sample

- `modules/sample-hello/module.yaml`
- `modules/sample-hello/README.md`
- `modules/sample-hello/frontend/contributions.yaml`
- `modules/sample-hello/services/README.md`
- `modules/sample-hello/tests/module-smoke.md`

The stage also adds SDK docs, schema docs, `ojosctl module init` and compatibility harness tooling.

## Core Files Not Changed For Sample Runtime Integration

The sample module integration does not require changes to:

- Kernel installer core validation logic for new sample-specific fields.
- Kernel installer service install/enable/disable logic.
- Gateway core route registration for sample routes.
- Gateway dynamic proxy matching logic.
- Web Shell main menu hardcoding.
- Topology page hardcoding.
- Permission page hardcoding.

## How It Enters Runtime

Installer stores the sample `module.yaml` in module registry tables. Kernel Runtime reads stored manifest metadata and derives Runtime Snapshot contributions. Web Shell and admin pages consume Runtime Snapshot and registry APIs.

Manifest contributions flow as follows:

- `permissions` -> permission registry.
- `menus` -> menu contribution registry.
- `frontend_routes` -> contribution registry and safe fallback route metadata.
- `gateway_routes` -> runtime route table viewer; disabled route is not proxied.
- `services` / `workers` -> runtime services API and topology nodes.
- `health_checks` -> health metadata.
- `topology.nodes` / `topology.edges` -> topology graph.

## Future Kernel Evolution Still Needed

Kernel evolution is still required for new extension point types, new service runtime drivers, dynamic frontend bundle execution, remote marketplace trust, hook execution, package signing policy and full hotplug automation.

Ordinary metadata/service/route/menu/permission/topology modules fit schema v1 and do not require Kernel/Gateway/Web Shell core edits.
