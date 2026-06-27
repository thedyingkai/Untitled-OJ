# OJOS

OJOS is a modular Online Judge platform baseline. It is organized around a Kernel, Gateway, Web Shell, Judge Core feature module, Module Installer, Runtime Snapshot and module SDK.

This repository is not a production-complete full-hotplug release. It is a kernel baseline that can be regression tested before starting the first real business module.

## Current Position

Implemented baseline capabilities:

- Auth, Problem API, Judge API and Judge Worker.
- Kernel Installer Core with manifest validation, package verification and local metadata lifecycle.
- Module Registry and Runtime Snapshot v1.
- Dynamic Gateway route table and trusted dynamic proxy.
- Web Shell contribution registry for menu, route metadata, permissions, topology and admin contribution views.
- Service Runtime Driver foundation and controlled apply through `ojosctl` / operator.
- Module Contract v1, Module SDK docs, `ojosctl module init` and `modules/sample-hello`.
- Compatibility harness proving ordinary metadata modules can install/enable/disable without sample-specific Kernel/Gateway/Web Shell core changes.

Not complete:

- L3 dynamic frontend bundles.
- Hooks.
- Remote module market.
- Package publisher signature / trust policy.
- Full service runtime automation.
- True multi-machine runtime apply.
- Full hotplug.
- Judge Core GA.

## Start Locally

Prepare `.env` from `.env.example`, then start the control plane with Docker Compose:

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build
```

Gateway is exposed on `http://localhost:8080`.

## Kernel Acceptance

Run the unified local acceptance gate:

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
```

The acceptance script calls:

- `scripts/verify-static.ps1`
- `scripts/e2e-api.ps1`
- `scripts/e2e-module-compat.ps1`
- `ojosctl` smoke commands

Controlled apply is not run by default. It must be explicitly requested:

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -RunControlledApply
```

## Static Verification

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

This runs Go tests, Rust tests, CLI smoke checks, frontend build, compose config checks and security scans unless skipped with script flags.

## Module SDK

Create a safe metadata-only module:

```powershell
cargo run -p ojosctl -- module init ojos.sample-hello --name "Sample Hello" --kind feature --out modules/sample-hello --with-topology
```

Validate and package:

```powershell
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
```

Temporary packages and scratch files must stay under `.tmp/agent/` and must not be committed.

## Runtime Snapshot

After the Docker control plane is running, inspect runtime snapshot through Gateway with an admin token:

```powershell
Invoke-RestMethod http://localhost:8080/api/admin/modules/runtime-snapshot
```

`include_disabled=true` is admin-only and is for registry inspection, compatibility checks and debugging.

## Controlled Apply Warning

Gateway and Web Shell do not apply runtime plans. Only `ojosctl` / operator controlled apply is allowed:

```powershell
cargo run -p ojosctl -- runtime plan-restart problem-api --out .tmp/agent/scratch/problem-api-restart.json
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --dry-run
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --confirm
```

Real apply requires explicit confirmation and only targets trusted compose allowlist services.

## Documentation

- [Documentation Index](docs/DOCS_INDEX.md)
- [Documentation Status](docs/DOCS_STATUS.md)
- [Kernel Baseline Freeze](docs/release/kernel-baseline-freeze.md)
- [Pre-Feature Gate](docs/release/pre-feature-gate.md)
- [Acceptance Matrix](docs/release/acceptance-matrix.md)
- [Regression Matrix](docs/release/regression-matrix.md)
- [Kernel Security Review](docs/security/kernel-security-review.md)
- [Module Contract v1](docs/modules/module-contract-v1.md)
- [Module SDK](docs/modules/module-sdk.md)
- [Judge Core Readiness](docs/modules/judge-core-readiness.md)

## Next Stage Gate

The next feature module may start only after the pre-feature gate is green:

- `acceptance-kernel` passes.
- `verify-static` passes.
- `e2e-api` passes.
- `e2e-module-compat` passes.
- Go/Rust/Frontend checks pass.
- `path_leaks=0`.
- ordinary user receives `403`.
- no token receives `401`.

Starting Contest work still requires a separate feature plan. This baseline does not start Contest API or Contest frontend work.
