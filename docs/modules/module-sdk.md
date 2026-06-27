# Module SDK

The Module SDK is the authoring surface for ordinary OJOS modules. It consists of the schema v1 contract, `ojosctl` commands, package verification and compatibility harnesses.

## Create A Module

```powershell
cargo run -p ojosctl -- module init ojos.sample-hello --name "Sample Hello" --kind feature --out modules/sample-hello --with-topology
```

Generated modules are metadata-only by default. They do not contain hooks, scripts, dynamic frontend bundles, Docker images, host mounts or privileged runtime options.

## Validate And Package

```powershell
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
```

## Install And Enable

Use Gateway admin APIs or the Web Shell Module Installer page:

```text
POST /api/admin/modules/validate
POST /api/admin/modules/install  dry_run=true
POST /api/admin/modules/install  dry_run=false
POST /api/admin/modules/:id/enable
POST /api/admin/modules/:id/disable
```

## Inspect Runtime

```text
GET  /api/admin/modules/runtime-snapshot
GET  /api/admin/modules/runtime-snapshot?include_disabled=true
GET  /api/admin/modules/runtime/routes?include_disabled=true
GET  /api/admin/runtime/services
POST /api/admin/runtime/services/:id/plan-start
```

## Controlled Apply

For trusted managed compose services, generate a plan and apply through `ojosctl`:

```powershell
cargo run -p ojosctl -- runtime plan-restart problem-api --out .tmp/agent/scratch/problem-api-restart.json
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --dry-run
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --confirm
```

Metadata services cannot be applied.

## Forbidden In v1

- No arbitrary `target_url`.
- No `command`, `script`, `hook`, `postinstall` or `preinstall`.
- No arbitrary `image`, `mount`, `host_path`, `privileged` or `cap_add`.
- No dynamic untrusted frontend JavaScript.
- No Gateway auth bypass.
- No reserved prefix takeover.
- No remote module market.
