# OJOS Documentation Status

Date: 2026-06-27

## Baseline Status

Kernel Baseline Freeze is implemented in this working tree and pending final verification. The baseline includes Installer Core, Module Registry, Runtime Snapshot v1, Dynamic Gateway route table/proxy, Web Shell contribution registry, Permission registry, Health aggregation, Service Runtime Driver foundation, controlled `ojosctl` apply, Module SDK and Sample Hello compatibility.

## Current Hotplug Status

| Level | Status | Notes |
| --- | --- | --- |
| L0 Metadata Hotplug | Complete | Registry, Runtime Snapshot, permissions, menus, topology metadata and health metadata |
| L1 Route/Menu/Topology/Permission Hotplug | Basically complete | Trusted dynamic route table/proxy and Web Shell contribution registry |
| L2 Service Runtime Foundation + Controlled Apply | Foundation complete | Services/workers, route-health linkage, plans and `ojosctl` controlled apply |
| L3 Dynamic Frontend Extension | Not complete | No untrusted dynamic JS or frontend bundle loading |
| L4 Full Module Hotplug | Not complete | No remote market, hooks or full service automation |

## Required Green Gate

The pre-feature gate requires:

- `scripts/acceptance-kernel.ps1 -SkipDockerBuild`
- `scripts/verify-static.ps1 -SkipDockerBuild`
- `scripts/e2e-api.ps1`
- `scripts/e2e-module-compat.ps1`
- Go `go test ./...`
- Rust root `cargo fmt --check`, `cargo check`, `cargo test`
- Judge worker `cargo fmt --check`, `cargo check`, `cargo test`
- Frontend `npm run build`

Required results:

- `failed=0`
- `path_leaks=0`
- `admin_health_status=ok`
- `admin_health_judge_status=ok`
- `sample_module_compat=passed`
- ordinary user `403`
- no token `401`

## Contract Status

| Contract | Status |
| --- | --- |
| Module manifest schema v1 | Frozen compatibility start |
| Runtime Snapshot v1 | Current source of truth for module contributions |
| `.ojosmod` package format v1 | Checksum integrity only |
| Package signature/trust policy | Not complete |

## Judge Core Status

Judge Core is the first core feature module and appears through Runtime Snapshot, routes, services and topology. Judge Core disable/uninstall remains protected. Judge Core is not GA; true multi-machine validation, network failure recovery, clock drift checks and long soak tests remain incomplete.

## Explicitly Out Of Scope

- B Contest implementation.
- Contest API.
- Contest frontend.
- Remote module market.
- Hook execution.
- Dynamic untrusted frontend JavaScript.
- Full hotplug automation.
- Judge Core GA.
