# Production Readiness Evidence

This matrix separates proven gates, local drill evidence, and newly configured drills that still need their first remote run.

## Current Evidence

| Capability | Gate | Status | Evidence |
| --- | --- | --- | --- |
| Redis live integration | ci | passed | Orchestrator CI: `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28621889030`. |
| MinIO live integration | ci | passed | Orchestrator CI: `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28621889030`. |
| Docker E2E | ci | passed | Orchestrator Docker E2E: `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28621889035`. |
| nsjail verdict matrix | ci | passed | Strict judge-worker nsjail live tests require real nsjail. |
| sandbox hardening | ci | passed | seccomp policy, mount whitelist, cgroup policy, runtime lock, and live nsjail tests. |
| staging backup/restore/rollback | nightly | pending-first-run; local passed | `deploy/ops/staging-drill.sh`, `Staging Drill` workflow. Local real restore verified: `artifacts/staging-drill-local-10/manifest.json`. |
| gateway browser E2E | ci | passed | Playwright test with trace/screenshot/video artifacts; local and Orchestrator CI passed. |
| manager GUI/TUI operator smoke | nightly | pending-first-run; local passed | `deploy/ops/manager-smoke.sh` records `manager_auth=deferred` and read-only/dev-ops beta mode. Local evidence: `artifacts/manager-smoke-local-3/manifest.json`. |
| alert firing | nightly | pending-first-run; local passed | Prometheus + Alertmanager webhook drill. Local evidence: `artifacts/alert-firing-drill-local-3/manifest.json`. |
| trace E2E | nightly | pending-first-run; local passed | `deploy/ops/trace-e2e-drill.sh` queries Jaeger for gateway-service, judge-api-service, storage-service, and judge-worker. Local evidence: `artifacts/trace-e2e-drill-local-9/manifest.json`. |
| secret policy | ci | passed | Redis password and `.env.production.example` production fail-fast policy added; local `deploy/ops/ci-policy.sh` and Orchestrator CI passed. |
| image build evidence | nightly | pending-first-run | Scheduled Docker build uploads image evidence. |
| service credential lifecycle | nightly | pending-first-run; local passed | Allow/deny/revoke/expire matrix local evidence: `artifacts/service-credential-drill-local-3/manifest.json`. |
| Redis recovery | nightly | pending-first-run; local passed | Pending/claim/AOF restart and judge-api queue status API local evidence: `artifacts/redis-recovery-drill-local-3/manifest.json`. |
| MinIO sample restore | nightly | pending-first-run; local passed | Covered by staging drill MinIO object restore plus storage-service readback: `artifacts/staging-drill-local-10/manifest.json`. |
| load/soak | nightly | pending-first-run; local passed | `deploy/ops/basic-load-soak.sh` covers auth login, problem list, storage put/get, judge submit, and result query. Local evidence: `artifacts/basic-load-soak-local-2/manifest.json`. |

## Secret Lifecycle

| Secret | Dev default | Production policy | Rotation |
| --- | --- | --- | --- |
| `JWT_SECRET` | empty in `.env.example` | required, min 32, weak values rejected | supported by env/secret manager restart |
| `AUTH_INTERNAL_TOKEN` | empty in `.env.example` | required, min 32 | supported by env/secret manager restart |
| `ORCHESTRATOR_INTERNAL_TOKEN` | empty in `.env.example` | required, min 32 | supported by env/secret manager restart |
| `OJOS_WORKER_TOKEN` | empty in `.env.example` | required, min 32 | supported by env/secret manager restart |
| DB passwords | empty in `.env.example` | each service DB password required, DB URL must not use superuser | supported by DB credential rotation plus service restart |
| `REDIS_PASSWORD` | `DEV_ONLY_redis_password_not_for_production` | required, min 20, Redis URL must include password | supported by Redis/service restart |
| `MINIO_ROOT_PASSWORD` | empty in `.env.example` | required, min 32 | supported by MinIO credential rotation |
| `MINIO_ACCESS_KEY` / `MINIO_SECRET_KEY` | empty in `.env.example` | required, access min 8, secret min 32 | supported by MinIO credential rotation |

## Remaining Evidence Gaps

- Newly added nightly drills have local passing evidence, but still need first successful GitHub Actions artifacts before their gate status can be promoted from `pending-first-run` to `passed`.
- Trace E2E currently proves a real local compose submission through Jaeger with a Redis metadata boundary and a native judge-worker OTLP consumer span; it still needs the first scheduled `Orchestrator Docker E2E` artifact.
- Basic load/soak has a local smoke result only; it is not a capacity test and still needs the first scheduled `Orchestrator Docker E2E` artifact.
