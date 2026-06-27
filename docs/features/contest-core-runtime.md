# Contest Core Runtime And Topology Draft

Status: design draft only. No runtime service is added in this gate.
Date: 2026-06-27

## Runtime Strategy

Contest Core Skeleton should likely introduce a trusted `contest-api` service so the dynamic route path is realistic. If schedule risk is high, the very first skeleton can start metadata-only, but that would not validate API proxy behavior.

Recommended skeleton:

- `contest-api` as a managed compose service.
- `contest-scoreboard-worker` as metadata-only future worker.
- `contest-scoreboard-refresh` as disabled metadata scheduled job.

## Trusted Compose Boundary

If `contest-api` is implemented, deployment must add it to the trusted compose allowlist. The module manifest must not define arbitrary image, command, script, mount, host path, privileged or capability settings.

## Service Lifecycle

| Service | Lifecycle | Runtime | Apply |
| --- | --- | --- | --- |
| `contest-api` | managed | trusted compose | controlled apply through `ojosctl` only |
| `contest-scoreboard-worker` | metadata | metadata | apply blocked |

Gateway/Web can request plans and view state, but must not apply runtime plans.

## Health Checks

| Health Check | Target | Required |
| --- | --- | --- |
| `contest-api-health` | `GET /healthz` on `contest-api` | yes if service exists |

Route `/api/contest` should be degraded or unavailable if `contest-api` is not `RUNNING` or health is not ok.

## Runtime Route Table

| Prefix | Service ID | Auth Mode | Permission |
| --- | --- | --- | --- |
| `/api/contest` | `contest-api` | `user` | `contest.view` |

## Topology Draft

```text
ojos.contest-core -> service:contest-api -> route:/api/contest
service:contest-api -> health:contest-api-health
ojos.contest-core -> worker:contest-scoreboard-worker
ojos.contest-core -> module:ojos.judge-core
ojos.contest-core -> storage:contest-exports
```

## Kernel Change Assessment

Contest Core Skeleton should not require Kernel changes if it stays inside Module Contract v1. Kernel evolution may be needed later for dynamic frontend bundles, new event delivery semantics, package trust signatures or new runtime drivers.
