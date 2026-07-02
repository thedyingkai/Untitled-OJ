# Production Readiness Evidence

This matrix separates proven gates from newly configured drills that still need their first remote run.

## Current Evidence

| Capability | Gate | Status | Evidence |
| --- | --- | --- | --- |
| Redis live integration | ci | passed | Orchestrator CI sets `OJOS_REAL_REDIS_URL` and runs Go Redis live tests. |
| MinIO live integration | ci | passed | Orchestrator CI starts pinned MinIO and runs storage-service real integration. |
| Docker E2E | nightly | passed | Orchestrator Docker E2E: `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28607098095`. |
| nsjail verdict matrix | ci | passed | Strict judge-worker nsjail live tests require real nsjail. |
| sandbox hardening | ci | passed | seccomp policy, mount whitelist, cgroup policy, runtime lock, and live nsjail tests. |
| staging backup/restore/rollback | nightly | pending-first-run | `deploy/ops/staging-drill.sh`, `Staging Drill` workflow. |
| gateway browser E2E | ci | pending-first-run | Playwright test with trace/screenshot/video artifacts; local `npm run test:e2e` passed before first CI run. |
| alert firing | nightly | pending-first-run | Prometheus + Alertmanager webhook drill. |
| trace E2E | deferred | deferred | No completed Jaeger query drill yet. |
| secret policy | ci | pending-first-run | Redis password and `.env.production.example` production fail-fast policy added; local `deploy/ops/ci-policy.sh` passed before first CI run. |
| image build evidence | nightly | pending-first-run | Scheduled Docker build uploads image evidence. |
| load/soak | deferred | deferred | No real staging load/soak evidence yet. |

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

- Jaeger trace E2E still needs a real submission trace queried from Jaeger.
- Basic load/soak still needs a real staging run with success rate, p50/p95, errors, queue pending max, and worker processed count.
- Newly added CI/nightly drills need first successful GitHub Actions artifacts before their gate status can be promoted from `pending-first-run` to `passed`.
