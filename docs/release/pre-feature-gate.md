# Pre-Feature Gate

This gate must be checked before starting the first real business module after Module SDK Compatibility Harness v1.

## Required Green Checks

- `scripts/acceptance-kernel.ps1 -SkipDockerBuild` passes.
- `scripts/verify-static.ps1 -SkipDockerBuild` passes.
- `scripts/e2e-api.ps1` passes with `failed=0`, `path_leaks=0`, `admin_health_status=ok`, `admin_health_judge_status=ok`.
- `scripts/e2e-module-compat.ps1` passes with `sample_module_compat=passed`.
- Go `go test ./...` passes for `services/shared`, `services/auth`, `services/gateway`, `services/problem-api`, `services/judge-api`.
- Rust root `cargo fmt --check`, `cargo check`, `cargo test` pass.
- Judge worker `cargo fmt --check`, `cargo check`, `cargo test` pass.
- Frontend `npm run build` passes.
- `module-installer` is healthy in the local compose environment when Docker e2e is used.

## Contract Gate

- All checked-in module manifests use `schema_version: 1`.
- Module Contract v1 remains strict: unknown top-level fields and unknown `provides` fields are rejected.
- Dangerous fields remain rejected: `command`, `script`, `hook`, `image`, `mount`, `host_path`, `privileged`, `cap_add`, `target_url`, secrets and token-like fields.
- `.ojosmod` package format remains version `1` with checksum integrity only.
- Runtime Snapshot remains version `1`.

## Security Gate

- Gateway/Web do not apply runtime plans.
- Gateway/Web/module-installer do not mount Docker socket.
- Dynamic Gateway proxy uses trusted `service_id`, not manifest-provided URLs.
- Reserved prefixes remain protected.
- Raw `Authorization` is not forwarded to module services by default.
- Controlled apply is only through `ojosctl` or a future operator and requires explicit confirmation.
- Path leak scans remain at `0`.

## Feature Start Decision

Starting a first real feature module is allowed only when the acceptance gate is green. The first module must stay inside Module Contract v1 unless a new extension point is explicitly designed and reviewed.

## Still Forbidden At This Gate

- Starting B Contest without a separate feature plan.
- Writing Contest API or Contest frontend as part of the freeze.
- Remote module market.
- Hook execution.
- Dynamic untrusted frontend JavaScript.
- Marking Judge Core GA.
- Claiming full hotplug.
