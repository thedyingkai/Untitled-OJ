> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# Deploy Control Plane

The Control Plane contains Gateway, Auth, Problem API, Judge API, PostgreSQL,
Redis, artifact storage and the frontend entrypoint.

## Start

```bash
cp .env.example .env
vi .env
docker compose --env-file .env -f deploy/compose/docker-compose.yml up -d --build
```

Required production changes:

```env
OJOS_WORKER_TOKEN=<openssl-rand-hex-32>
JWT_SECRET=<random-jwt-secret>
POSTGRES_PASSWORD=<strong-password>
POSTGRES_DSN=postgres://postgres:<strong-password>@postgres:5432/ojos?sslmode=disable
REDIS_URL=redis://redis:6379/0
```

The service YAML files under `services/*/etc` intentionally do not contain
production DSNs or secrets. Runtime services read `DATABASE_URL` or
`POSTGRES_DSN`, `REDIS_URL`, `JWT_SECRET`, storage roots and worker token from
the environment supplied by Compose or the process manager.

## Services

| Service | Exposure | Notes |
| --- | --- | --- |
| Gateway | Public `8080` | Only public HTTP entrypoint |
| Auth | Internal only | Proxied by Gateway |
| Problem API | Internal only | Proxied by Gateway and protected by internal HMAC |
| Judge API | Internal only | Proxied by Gateway and protected by internal HMAC |
| PostgreSQL | Internal only | Not exposed to workers or the public network |
| Redis | Internal only | Signal history for submissions; PostgreSQL owns task leases |
| Worker | Optional local worker | Uses Worker Link through Gateway |

## Worker Link

Set `OJOS_WORKER_TOKEN` on judge-api and every worker node. Worker endpoints are:

```text
POST /api/judge/worker/register
POST /api/judge/worker/heartbeat
POST /api/judge/worker/tasks/claim
POST /api/judge/worker/tasks/:task_id/heartbeat
POST /api/judge/worker/tasks/:task_id/result
POST /api/judge/worker/tasks/:task_id/fail
GET  /api/judge/worker/artifacts/submissions/:id/source
GET  /api/judge/worker/artifacts/problems/:id/package
```

Worker API requests must include:

```http
X-OJOS-Worker-Token: <token>
```

## Artifact Storage

Local development uses local filesystem storage:

```text
storage/problems
storage/submissions
```

Remote workers never receive these paths. Judge API serves source files and
zipped problem packages through the Worker Artifact API with sha256 digests.
Workers verify the digest after download.

In the provided compose file, Gateway mounts both artifact roots read-only so
`/api/admin/health` can check storage reachability. Judge API mounts
submissions read-write and problems read-only so it can serve worker artifacts.

## Apply Migrations

Run migrations against the internal PostgreSQL endpoint from a trusted admin
host or a one-shot migration container. Example from the host in development:

```bash
migrate -path deploy/migrations \
  -database "$POSTGRES_DSN" up
```

## Security Notes

- Do not expose PostgreSQL or Redis to the public network.
- Do not expose problem-api or judge-api host ports.
- Rotate `OJOS_WORKER_TOKEN` if a worker node is compromised.
- Keep Gateway as the only public API entrypoint.
- Do not configure judge-worker with DB or Redis credentials.

## Verification

Static verification:

```bash
powershell -NoProfile -File scripts/verify-static.ps1 -SkipDockerBuild
```

With Docker daemon available:

```bash
powershell -NoProfile -File scripts/verify-static.ps1
```
