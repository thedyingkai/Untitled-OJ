# Release Candidate Evidence

## Verdict

CONDITIONAL GO for the first production candidate / beta release.

Reason: P0 is zero after the staging drill config fix and the RC formal-docs allowlist fix. Core CI, Docker E2E, nsjail verdict matrix, sandbox abuse tests, browser E2E, secret policy, local staging recovery, local observability drills, local trace E2E, local image build, and local basic load/soak all pass. The release remains conditional because the newly fixed nightly staging/ops/image/trace/load gates still need their first successful remote artifacts after `853423a`.

## Candidate

| Field | Value |
| --- | --- |
| Validated code commit | `853423a80d2ba20840867b4420a4f70da57b34af` |
| Evidence commit | this checked-in evidence update; final exact hash is reported by `git rev-parse HEAD` |
| Generated on | 2026-07-03 Asia/Shanghai |
| Recommendation | CONDITIONAL GO |
| P0 count | 0 |
| Stable production scope | beta / first production candidate, not full HA capacity release |

## Gate Matrix

| Gate | Result | Type | Evidence |
| --- | --- | --- | --- |
| `cargo fmt --check` | passed | local | RC local run |
| `cargo test --workspace` | passed | local | 31 daemon, 12 GUI, 9 TUI, 176 core, pg integration all passed after adding the RC docs to the formal docs allowlist |
| Go tests: auth-service | passed | local | `go test ./...` |
| Go tests: gateway | passed | local | `go test ./...` |
| Go tests: storage-service | passed | local | `go test ./...` |
| Go tests: judge-api | passed | local | `go test ./...` |
| Go tests: problem-service | passed | local | `go test ./...` |
| Go tests: user-service | passed | local | `go test ./...` |
| judge-worker cargo test | passed | local | 25 tests, including nsjail matrix and sandbox abuse tests |
| Docker Compose config | passed | local | `docker compose -f deploy/compose/docker-compose.yml config --quiet` |
| gateway frontend build | passed | local | `npm run build` |
| gateway browser E2E | passed | local / ci | local `npm run test:e2e`; CI `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| `git diff --check` | passed | local | RC local run |
| Redis live integration | passed | ci | `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| MinIO live integration | passed | ci | `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| Orchestrator Docker E2E | passed | ci | `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416062` |
| nsjail verdict matrix | passed | ci / local | CI and local judge-worker tests |
| sandbox abuse tests | passed | ci / local | CI and local judge-worker tests |
| production secret fail-fast | passed | ci / local | CI and local `deploy/ops/ci-policy.sh` |
| backup -> restore -> rollback drill | pending-first-run; local passed | nightly / local | `artifacts/rc-staging-drill-2/manifest.json` |
| service credential lifecycle | pending-first-run; local passed | nightly / local | `artifacts/rc-service-credential-drill/manifest.json` |
| Redis recovery drill | pending-first-run; local passed | nightly / local | `artifacts/rc-redis-recovery-drill/manifest.json` |
| MinIO restore drill | pending-first-run; local passed | nightly / local | `artifacts/rc-staging-drill-2/manifest.json` |
| alert firing drill | pending-first-run; local passed | nightly / local | `artifacts/rc-alert-firing-drill/manifest.json` |
| trace E2E | pending-first-run; local passed | nightly / local | `artifacts/rc-trace-e2e-drill/manifest.json` |
| image build | pending-first-run; local passed | nightly / local | `artifacts/rc-image-build/manifest.json` |
| basic load/soak smoke | pending-first-run; local passed | nightly / local | `artifacts/rc-basic-load-soak/manifest.json` |

## Module Readiness

| Module | Readiness | Notes |
| --- | ---: | --- |
| orchestrator-core/backend | 91% | CI, pg integration, release install/rollback model, registry route checks pass |
| judge-worker | 92% | nsjail matrix and abuse tests pass; not a formal sandbox proof |
| judge-api | 89% | Redis task queue, trace propagation, worker result path covered |
| auth-service | 88% | permission seed and credential lifecycle evidence present |
| gateway backend | 87% | proxy/auth/route checks pass |
| gateway frontend | 86% | browser E2E exists but minimal |
| problem-service | 85% | package validation and storage integration covered |
| storage-service | 88% | local/MinIO paths and tracing covered |
| user-service | 84% | basic service tests pass; smaller production surface |
| platform/shared | 86% | reused logging/tracing/middleware |
| manager GUI | 80% | operator smoke only; auth deferred |
| manager TUI | 80% | operator smoke only; auth deferred |
| deploy/ops | 86% | local drills pass; remote nightly first-success pending |
| PostgreSQL | 90% | live integration and backup/restore evidence |
| Redis | 87% | live integration and local recovery drill evidence |
| MinIO | 87% | live integration and local restore/readback evidence |
| Jaeger/observability | 84% | local alert and trace drills pass; coverage still narrow |
| sdk/sets/docs | 83% | release docs/checklists present; ongoing operator polish remains |

Engineering Maturity: 90%.
Stable Production Readiness: 85%.

Engineering maturity measures code structure, tests, contracts, and operational tooling. Stable production readiness discounts maturity for unproven remote drills, HA/capacity gaps, and accepted operational risks.

## P0/P1 Status

| Item | Severity | Status | Fix / risk |
| --- | --- | --- | --- |
| main CI red | P0 | cleared | CI passed at `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| compose production profile cannot config | P0 | cleared | local compose config passed |
| secret fail-fast weak defaults | P0 | cleared | `deploy/ops/ci-policy.sh` passed |
| judge-worker verdict matrix / sandbox abuse | P0 | cleared | CI and local tests passed |
| RC docs made `cargo test --workspace` fail formal docs allowlist | P0 | fixed | formal docs allowlist updated for RC evidence docs |
| staging drill storage-service config missing Jaeger | P1 | fixed | `853423a` |
| current nightly first-success artifacts pending | P1 | accepted risk | local current RC drills passed; wait for scheduled artifacts before GA |
| manager auth deferred | P1 | accepted risk | beta read-only/dev-ops mode only |
| alert/trace coverage narrow | P1 | accepted risk | one firing rule and one judge trace path only |
| schema rollback unsupported | P1 | accepted risk | app-level rollback only |
| load/soak is short smoke | P1 | accepted risk | not capacity evidence |

## Accepted Risks

- Nightly staging/ops/image/trace/load artifacts are pending first success after `853423a`; local RC evidence is available.
- Manager GUI/TUI are read-only/dev-ops beta with auth deferred.
- Schema rollback is unsupported; release rollback is app-level.
- Alert firing covers one synthetic rule only.
- Trace E2E covers the judge submission path, with Redis boundary represented by metadata/link semantics.
- Load/soak is a short smoke, not capacity planning.
- No HA/failover topology is claimed for this beta.

## Deferred Items

- P2: broaden browser E2E coverage beyond minimal login/problem/submission/result paths.
- P2: add more observability rules and dashboards.
- P2: formal HA deployment pattern and failover drill.
- P2: longer load/soak and capacity envelope.
- P3: manager auth and richer operator workflows.
- P3: MinIO lifecycle/policy hardening beyond sample restore.
