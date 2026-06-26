> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# Deploy Worker Node

This document describes how to deploy a judge-worker on a separate machine.
The worker does not connect to PostgreSQL or Redis and does not mount the
control-plane storage directory. It only makes outbound HTTPS/HTTP requests to
the Control Plane Worker API.

## Requirements

- Linux host with cgroup v2 enabled.
- Docker Engine and Docker Compose.
- Network egress from the worker node to `OJOS_CONTROL_PLANE_URL`.
- `nsjail` runs inside the worker container.
- The host must expose `/sys/fs/cgroup` to the container as read-write.

Check cgroup v2:

```bash
test -f /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.controllers
```

Expected controllers include `memory` and `pids`.

## Worker Token

Generate one token per deployment environment:

```bash
openssl rand -hex 32
```

Set the same value on the Control Plane as `OJOS_WORKER_TOKEN` and on each
worker node in `deploy/worker/.env`.

## Control Plane Configuration

On the Control Plane:

```bash
cp .env.example .env
vi .env
docker compose --env-file .env -f deploy/compose/docker-compose.yml up -d --build
```

The Gateway must expose only `8080` to worker nodes. PostgreSQL, Redis,
problem-api and judge-api stay on the internal Docker network.

## Worker Node Configuration

On each worker machine:

```bash
git clone <repo-url> ojos
cd ojos
cp deploy/worker/.env.example deploy/worker/.env
vi deploy/worker/.env
```

Example:

```env
OJOS_WORKER_ID=worker-node-02
OJOS_WORKER_NAME=Worker Node 02
OJOS_CONTROL_PLANE_URL=https://ojos.example.com/api
OJOS_WORKER_TOKEN=the-same-token-configured-on-control-plane
OJOS_MAX_CONCURRENCY=2
```

Start:

```bash
docker compose --env-file deploy/worker/.env -f deploy/worker/docker-compose.yml up -d --build
```

## Verify Registration

Log in as an admin and open:

```text
/admin/judge
```

Expected:

- Worker appears in the Workers table.
- Status is `ONLINE`.
- Supported languages match `OJOS_SUPPORTED_LANGUAGES`.

You can also query the API through Gateway with an admin token:

```bash
curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$OJOS_PUBLIC_BASE_URL/api/judge/admin/workers"
```

## Task Flow

1. Worker registers through `POST /api/judge/worker/register`.
2. Worker heartbeats through `POST /api/judge/worker/heartbeat`.
3. Worker claims leases through `POST /api/judge/worker/tasks/claim`.
4. Worker downloads source and problem package through artifact API URLs.
5. Worker verifies sha256 digest before judging.
6. Worker heartbeats the task lease while judging.
7. Worker uploads result through `POST /api/judge/worker/tasks/:task_id/result`.

Old task leases cannot update a submission after a newer lease exists.

## Local Two-Worker Simulation

When a second machine is not available, simulate two workers on one Linux host:

```bash
docker compose --env-file .env -f deploy/compose/docker-compose.yml up -d --scale judge-worker=2 judge-worker
```

Then open `/admin/judge` or call:

```bash
curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$OJOS_PUBLIC_BASE_URL/api/judge/admin/workers"
```

Expected: two worker entries with distinct container hostnames. Submit several
jobs and verify `/api/judge/admin/tasks` shows work assigned without duplicate
claims for the same submission.

## Drain And Upgrade

Drain from the admin page or API:

```bash
curl -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$OJOS_PUBLIC_BASE_URL/api/judge/admin/workers/worker-node-02/drain"
```

The worker stops receiving new tasks. Existing tasks continue until complete.

Upgrade:

```bash
docker compose --env-file deploy/worker/.env -f deploy/worker/docker-compose.yml pull
docker compose --env-file deploy/worker/.env -f deploy/worker/docker-compose.yml up -d --build
```

## Failure Recovery

- If a worker disappears, `last_seen` becomes stale and the admin page shows it
  as `OFFLINE`.
- Running tasks have `lease_expires_at`.
- Expired running leases are reset to `PENDING` by the claim path before new
  tasks are assigned.
- A different worker can claim the recovered task.
- If the old worker uploads after recovery, `worker_id` and `lease_version`
  mismatch and the result is rejected.

## Runtime Acceptance Script

On a Linux host with Docker daemon and cgroup v2:

```bash
bash scripts/e2e-linux.sh
```

The script starts the control plane, creates an A+B problem, starts workers,
submits AC/WA/CE/RE/TLE/MLE/OLE programs for supported languages, and checks
admin queue/health APIs. It does not mark checks as passed if Docker, nsjail or
cgroup v2 are unavailable.

## Security Boundary

- Worker nodes do not receive PostgreSQL credentials.
- Worker nodes do not receive Redis credentials.
- Worker nodes do not mount `storage/problems` or `storage/submissions`.
- Worker API requires `X-OJOS-Worker-Token`.
- Gateway still signs worker requests to internal judge-api with internal HMAC.
- Artifact downloads require a valid task lease.
- The worker container does not enable Docker privileged mode.
- The default worker compose does not disable seccomp or AppArmor. Use a
  reviewed custom profile only if your nsjail build needs one.
