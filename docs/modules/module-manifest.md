# Module Manifest

> Status: current implementation, `schema_version: 1`
> Last updated: 2026-06-27

OJOS modules are declared through `modules/<module>/module.yaml`. The manifest is a contract between a module package and the Kernel. It must describe capabilities; it must not contain secrets or executable hooks.

## Base Shape

```yaml
schema_version: 1

id: ojos.demo-module
name: Demo Module
version: 0.1.0
set: demo
kind: feature
status: demo
description: Installer validation demo module.

compatibility:
  platform: ">=0.1.0"
  installer: ">=0.1.0"

requires:
  modules:
    - id: ojos.platform.web-shell
      version: ">=0.1.0"
    - id: ojos.platform.identity-access
      version: ">=0.1.0"
    - id: ojos.kernel.module-runtime
      version: ">=0.1.0"

provides:
  permissions:
    - key: demo.view
      description: View demo module metadata.
  roles: []
  components: []
  services: []
  workers: []
  frontend_routes: []
  menus: []
  gateway_routes: []
  storage:
    buckets: []
  storage_buckets: []
  health_checks: []
  migrations: []
  events:
    publishes: []
    subscribes: []
  scheduled_jobs: []
  admin_panels: []
  topology:
    nodes: []
    edges: []
```

## Validation

Rust installer core validates:

- `id` matches `[a-z0-9][a-z0-9.-]*`.
- `version` is semver.
- `schema_version` is supported.
- `kind` is `kernel`, `platform`, `feature`, `integration`, or `metadata`.
- `status` is `builtin`, `external`, or `demo`.
- permissions, roles, components, services, workers, routes, menus, gateway prefixes, buckets, health checks, jobs, admin panels, topology nodes, and dependencies are not duplicated.
- dependencies are not self references.
- migrations are relative `deploy/migrations/*.sql` paths and `up/down` pairs match.

## Forbidden Fields

Manifest content must not contain fields named:

```text
secret
token
password
private_key
env
command
script
hook
postinstall
preinstall
remote_url
download_url
```

v0 does not execute hooks, does not download remote modules, and does not load dynamic frontend bundles.

## Path Safety

`validate_manifest_file` requires:

- manifest path is relative
- manifest is under repo root `modules/`
- canonical path remains under `modules/`
- file name is `module.yaml`
- no `..`, absolute path, symlink escape, `.tmp`, `.env`, `node_modules`, `frontend/dist`, `target`, or `.git`

## Signature Fields

Reserved fields:

```yaml
signature:
signing_key_id:
trusted_publisher:
```

v0 only verifies checksum integrity for local packages. Signature and publisher trust policy are v1 work, so v0 must not install remote untrusted modules automatically.
