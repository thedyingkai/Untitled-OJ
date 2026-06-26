> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# E2E Engineering Acceptance

This document is executable-first. Do not mark a step as passed unless the
command has run successfully in the target environment.

## 1. Static Build And Safety Scan

Run on Windows PowerShell or PowerShell Core:

```powershell
pwsh -File scripts/verify-static.ps1 -SkipDockerBuild
```

Expected:

- Go format check passes.
- `go build ./...` and `go test ./...` pass for shared/auth/gateway/problem-api/judge-api.
- `cargo fmt --check` and `cargo check` pass for judge-worker.
- `npm run build` passes.
- Compose config renders for control-plane and worker.
- Public schema and frontend scans do not expose internal storage paths.

Troubleshooting:

- If Docker daemon is available, rerun without `-SkipDockerBuild`.
- If a scan fails, inspect the printed file/line before continuing.

## 2. Full Linux Runtime Acceptance

Run on a Linux host with Docker daemon, Docker Compose, `jq`, `curl`, cgroup v2
and enough privileges for nsjail in the worker container:

```bash
OJOS_ADMIN_USERNAME=admin1 \
OJOS_ADMIN_PASSWORD=admin123 \
OJOS_USER_A=user-a \
OJOS_USER_B=user-b \
OJOS_USER_PASSWORD=user123 \
OJOS_WORKER_TOKEN="$(openssl rand -hex 32)" \
bash scripts/e2e-linux.sh
```

Expected:

- Control Plane builds and starts.
- A normal user can log in.
- An admin token is available through `OJOS_ADMIN_TOKEN` or the configured admin account.
- A public A+B problem is created through API.
- Package validation runs through API.
- Two local worker containers can be started with Worker Link.
- Worker registration is visible through `/api/judge/admin/workers`.
- Submissions reach terminal states.
- Queue and health APIs return real data.

Troubleshooting:

- If admin login fails, create the admin user and grant `super_admin` from a
  trusted DB console, then rerun with `OJOS_ADMIN_TOKEN`.
- If worker registration fails with 401, verify `OJOS_WORKER_TOKEN` matches on
  judge-api and workers.
- If MLE is reported as another status, verify `/sys/fs/cgroup/cgroup.controllers`
  includes `memory` and the worker can write to the configured cgroup root.

## 3. Migrations

Apply migrations before runtime checks:

```bash
migrate -path deploy/migrations -database "$POSTGRES_DSN" up
```

Expected:

- `judge_workers` and `judge_tasks` exist.
- permission tables and `internal_auth_keys` exist.

Troubleshooting:

- Verify `POSTGRES_DSN` is reachable from the migration host.
- Do not expose PostgreSQL publicly for migration convenience.

## 4. Admin Bootstrap

Create the first admin user through the public API:

```bash
curl -sS -X POST "$OJOS_PUBLIC_BASE_URL/api/auth/register" \
  -H "Content-Type: application/json" \
  -d '{"username":"admin1","password":"admin123"}'
```

Grant `super_admin` from a trusted DB console:

```sql
INSERT INTO user_roles(user_id, role_id)
SELECT u.id, r.id
FROM users u, roles r
WHERE u.username = 'admin1'
  AND r.name = 'super_admin'
ON CONFLICT DO NOTHING;
```

Expected:

- The admin can access `/admin/health`, `/admin/judge`, `/admin/users`,
  `/admin/permissions` and `/admin/permission-check`.

## 5. Problem And Package Validation

The Linux E2E script creates an A+B problem with:

```http
POST /api/problem/problems
POST /api/problem/problems/:id/test-cases
POST /api/problem/problems/:id/package/validate
```

Expected:

- `problem_id` is returned.
- Validation result does not include server filesystem paths.
- Missing input, missing answer, invalid score, invalid YAML and path escape
  cases can be reproduced by editing the generated package files on a trusted
  control-plane host and rerunning package validation.

## 6. Submission Status Matrix

`scripts/e2e-linux.sh` submits AC/WA/CE/RE/TLE/MLE/OLE programs for:

- `cpp17`
- `c11`
- `python3`
- `java17`

Expected terminal statuses:

| Program | Expected status |
| --- | --- |
| AC | `ACCEPTED` |
| WA | `WRONG_ANSWER` |
| CE | `COMPILE_ERROR` |
| RE | `RUNTIME_ERROR` |
| TLE | `TIME_LIMIT_EXCEEDED` |
| MLE | `MEMORY_LIMIT_EXCEEDED` |
| OLE | `OUTPUT_LIMIT_EXCEEDED` |

Troubleshooting:

- Check `/api/judge/submissions/:id`.
- Check `/api/judge/submissions/:id/cases`.
- Check `/admin/judge` for lease status.
- Check worker container logs.

## 7. Worker Crash Recovery

Run with two workers and stop one while it is judging:

```bash
docker stop <worker-container-a>
```

Expected:

- Its task lease expires.
- `judge_tasks.status` returns to `PENDING`.
- Another worker claims the task.
- A stale upload from the old lease is rejected because `worker_id` and
  `lease_version` no longer match.

Troubleshooting:

- Inspect `/api/judge/admin/tasks`.
- Verify `OJOS_TASK_LEASE_TTL` is not too large for the test.

## 8. Redis Signal History

PostgreSQL is the task ownership source. Redis Streams are signal history and
are trimmed during `XADD`.

Commands:

```bash
docker exec ojos-redis redis-cli XLEN ojos:judge:submissions
docker exec ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
curl -sS -H "Authorization: Bearer $ADMIN_TOKEN" \
  "$OJOS_PUBLIC_BASE_URL/api/judge/admin/queue" | jq .
```

Expected:

- Stream length is bounded by the `MAXLEN ~ 10000` strategy.
- Redis pending is diagnostic only.
- Stale judging recovery is driven by `judge_tasks.lease_expires_at`.

## 9. Permission Grant And Removal

Grant problem owner:

```bash
curl -sS -X POST "$OJOS_PUBLIC_BASE_URL/api/auth/admin/problems/roles" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"user_id":2,"problem_id":1,"role":"problem_owner"}'
```

Remove problem owner:

```bash
curl -sS -X DELETE "$OJOS_PUBLIC_BASE_URL/api/auth/admin/problems/roles" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"user_id":2,"problem_id":1,"role":"problem_owner"}'
```

Expected:

- Permission check for `problem.edit` on the problem changes from denied to
  allowed and back to denied.
- Audit entries appear in `/admin/permissions`.

## 10. Frontend Flow

Run:

```bash
cd frontend
npm run build
```

Open these routes through Gateway:

- `/login`
- `/register`
- `/dashboard`
- `/me`
- `/problems`
- `/problems/:id`
- `/problems/:id/submit`
- `/submissions/:id`
- `/admin/health`
- `/admin/judge`
- `/admin/users`
- `/admin/permissions`
- `/admin/permission-check`

Expected:

- Pages call real APIs.
- Unauthenticated users are redirected to login.
- Non-admin users receive 403 for admin routes.
- PENDING/JUDGING submissions poll until terminal status.
