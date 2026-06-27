# Kernel Acceptance Matrix

| Area | Script / command | Current status | Failure handling | Blocks next feature |
| --- | --- | --- | --- | --- |
| Static verification | `scripts/verify-static.ps1 -SkipDockerBuild` | Required green | Fix failing build/test/scan before merging | Yes |
| Docker API e2e | `scripts/e2e-api.ps1` | Required green in local Docker acceptance | Inspect `.tmp/agent/reports/api-runtime` and service logs | Yes |
| Module compatibility | `scripts/e2e-module-compat.ps1` | Required green | Inspect `.tmp/agent/reports/module-compat` | Yes |
| CLI smoke | `cargo run -p ojosctl -- module doctor`, scaffold, package, runtime plan dry-run | Required green through `verify-static` and `acceptance-kernel` | Fix CLI or contract drift | Yes |
| Controlled apply | `acceptance-kernel.ps1 -RunControlledApply` | Optional, explicit only | Do not run by default; fix operator path before claiming apply | No for plan-only features, yes for apply changes |
| Path leak scan | e2e summaries and static scans | Must be `0` | Redact output and remove internal path exposure | Yes |
| Permission rejection | e2e ordinary user/no token checks | Must be ordinary `403`, no token `401` | Fix auth boundary before merging | Yes |

`scripts/acceptance-kernel.ps1` is the unified local entry and records a summary containing `static_failed`, `api_failed`, `compat_failed`, `path_leaks`, `admin_health_status`, `admin_health_judge_status`, `module_compat`, `controlled_apply` and `overall_status`.
