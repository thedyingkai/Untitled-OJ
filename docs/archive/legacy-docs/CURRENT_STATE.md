> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# OJOS Current State

This document records the repository state after the Worker Link and admin
frontend work. It references real files and keeps unverifiable runtime checks
separate from implemented code.

## Backend Services

| Service | Entry | Config | Current capability |
| --- | --- | --- | --- |
| Gateway | `services/gateway/gateway.go` | `services/gateway/etc/gateway.yaml` | Public API entrypoint, JWT proxy auth, internal HMAC signing, admin health |
| Auth | `services/auth/auth.go` | `services/auth/etc/auth.yaml` | Register, login, profile/me, admin role/permission APIs, audit listing |
| Problem API | `services/problem-api/problemapi.go` | `services/problem-api/etc/problemapi.yaml` | Problem CRUD, package summary, package validation, package cases |
| Judge API | `services/judge-api/judgeapi.go` | `services/judge-api/etc/judgeapi.yaml` | Submissions, languages, debug logs, Worker Link, admin judge APIs |
| Judge Worker | `services/judge-worker/src/main.rs` | `services/judge-worker/config/languages.yaml` | Worker Link HTTP mode, nsjail, cgroup v2 resource limits |
| Shared | `services/shared` | Go module | DB pool, JWT, auth context, permission core, internal HMAC |

## Existing APIs

Gateway routes are configured in `services/gateway/etc/gateway.yaml`:

- `/api/auth/*` -> Auth, optional JWT.
- `/api/problem/*` -> Problem API, required JWT.
- `/api/judge/worker/*` -> Judge API worker-only route; worker token is verified by Judge API.
- `/api/judge/*` -> Judge API, required JWT.
- `/api/admin/health` -> Gateway admin health.

Auth APIs in `services/auth/auth.api`:

- `GET /health`
- `POST /auth/register`
- `POST /auth/login`
- `GET /auth/profile`
- `GET /auth/me`
- `GET /auth/admin/users`
- `GET /auth/admin/roles`
- `GET /auth/admin/permissions`
- `POST /auth/admin/users/roles`
- `DELETE /auth/admin/users/roles`
- `POST /auth/admin/problems/roles`
- `DELETE /auth/admin/problems/roles`
- `POST /auth/admin/permission-check`
- `GET /auth/admin/audit-logs`

Problem APIs in `services/problem-api/problemapi.api`:

- `GET /health`
- `POST /problem/problems`
- `GET /problem/problems`
- `GET /problem/problems/:id`
- `PUT /problem/problems/:id`
- `DELETE /problem/problems/:id`
- `GET /problem/problems/:id/package`
- `POST /problem/problems/:id/package/validate`
- `GET /problem/problems/:id/package/cases`
- `POST /problem/problems/:id/test-cases`
- `GET /problem/problems/:id/test-cases`
- `PUT /problem/problems/:id/test-cases/:case_no`
- `DELETE /problem/problems/:id/test-cases/:case_no`

Judge APIs in `services/judge-api/judgeapi.api` and
`services/judge-api/internal/handler/routes.go`:

- `GET /judge/languages`
- `POST /judge/submissions`
- `GET /judge/submissions`
- `GET /judge/submissions/:id`
- `GET /judge/submissions/:id/cases`
- `GET /judge/submissions/:id/debug-logs`
- `POST /judge/submissions/:id/cancel`
- `POST /judge/problems/:id/rejudge`
- `POST /judge/worker/register`
- `POST /judge/worker/heartbeat`
- `POST /judge/worker/tasks/claim`
- `POST /judge/worker/tasks/:task_id/heartbeat`
- `POST /judge/worker/tasks/:task_id/result`
- `POST /judge/worker/tasks/:task_id/fail`
- `GET /judge/worker/artifacts/submissions/:id/source`
- `GET /judge/worker/artifacts/problems/:id/package`
- `GET /judge/admin/queue`
- `GET /judge/admin/workers`
- `GET /judge/admin/tasks`
- `POST /judge/admin/workers/:id/drain`
- `POST /judge/admin/submissions/:id/requeue`

## Frontend Routes

Routes are defined in `frontend/src/router/index.ts`:

- `/login`
- `/register`
- `/dashboard`
- `/me`
- `/problems`
- `/problems/new`
- `/problems/:id`
- `/problems/:id/edit`
- `/problems/:id/package`
- `/problems/:id/submit`
- `/submissions`
- `/submissions/:id`
- `/admin/health`
- `/admin/judge`
- `/admin/users`
- `/admin/permissions`
- `/admin/permission-check`
- `/admin/problems/:id/permissions`
- `/403`
- `/500`
- catch-all `/404`

Admin pages now call real APIs:

- `frontend/src/views/admin/AdminHealthView.vue`
- `frontend/src/views/admin/AdminJudgeView.vue`
- `frontend/src/views/admin/AdminUsersView.vue`
- `frontend/src/views/admin/AdminPermissionsView.vue`
- `frontend/src/views/admin/AdminPermissionCheckView.vue`
- `frontend/src/views/admin/AdminProblemPermissionsView.vue`

`frontend/src/views/admin/AdminPendingView.vue` has been removed.

## Database And Migrations

Migrations live in `deploy/migrations`:

- `000001_init_schema.up.sql`: users, roles and user roles.
- `000002_judge_schema.up.sql`: initial problems/submissions schema.
- `000003_permission_core.up.sql`: permission tables and audit logs.
- `000004_problem_package_core.up.sql`: package fields and `problem_files`.
- `000005_judge_sandbox_storage_cleanup.up.sql`: source/result storage fields and cancellation fields.
- `000006_add_internal_auth_keys.up.sql`: internal HMAC key storage.
- `000007_problem_catalog_fields.up.sql`: problem catalog fields.
- `000008_worker_link.up.sql`: `judge_workers` and `judge_tasks`.

`judge_tasks` stores `submission_id`, `task_id`, `worker_id`,
`lease_version`, `lease_expires_at`, `heartbeat_at`, `attempt` and task
`status`.

## Redis Stream Usage

Redis stream constants are in `services/judge-api/internal/logic`:

- stream: `ojos:judge:submissions`
- diagnostic group name: `judge-workers`
- events: `submission.created`, `submission.requeued`

PostgreSQL is the task ownership source. Redis Streams are signal history only.
Worker nodes do not connect to Redis. Signals are written with approximate
`MAXLEN ~ 10000` trimming in `queue_signal.go`. Admin queue exposes stream
length and diagnostic pending data, but pending entries do not own or lock
tasks.

## Worker Connection Mode

`services/judge-worker/src/worker_link.rs` implements the worker protocol:

- reads `OJOS_CONTROL_PLANE_URL`;
- authenticates with `OJOS_WORKER_TOKEN`;
- registers and heartbeats;
- claims leases through Judge API;
- downloads source and problem package artifacts;
- verifies sha256;
- judges in local `OJOS_WORK_DIR`;
- uploads results through Judge API.

The worker module does not connect to PostgreSQL or Redis.

## Storage

Local development storage remains:

- `storage/problems`
- `storage/submissions`

Problem API writes problem packages. Judge API stores source/result artifacts
and serves bounded artifacts to workers through Worker Link. Public API schema
and frontend do not expose internal storage paths.

## Permissions

Permission core is in `services/shared/security/permission/permission.go`.
Auth profile returns roles and permissions. Frontend route guards are in
`frontend/src/router/index.ts`; `PermissionGuard.vue` provides component-level
gating.

Implemented:

- problem create/edit/delete/data management checks;
- judge submit and submission view checks;
- admin APIs require admin role or `system.admin`;
- user role binding APIs;
- problem-scoped role binding APIs;
- permission check API;
- audit log listing;
- worker API requires worker token.

## Internal HMAC

Gateway signing is implemented in `services/gateway/internal/proxy/proxy.go`.
Verification middleware exists in Problem API and Judge API. Worker paths pass
through Gateway and are then checked by Judge API worker-token middleware.

## Judge Resource Limits

Worker resource code:

- `services/judge-worker/src/sandbox.rs`
- `services/judge-worker/src/cgroup.rs`

Supported statuses:

- `ACCEPTED`
- `WRONG_ANSWER`
- `COMPILE_ERROR`
- `RUNTIME_ERROR`
- `TIME_LIMIT_EXCEEDED`
- `MEMORY_LIMIT_EXCEEDED`
- `OUTPUT_LIMIT_EXCEEDED`
- `SYSTEM_ERROR`
- `CANCELLED`
- `UNSUPPORTED_LANGUAGE`
- `PENDING`
- `JUDGING`

Languages configured in `services/judge-worker/config/languages.yaml`:

- `cpp17`
- `cpp20`
- `c11`
- `python3`
- `java17`

## Remaining Runtime Validation

The repository contains executable checks:

- Static/build checks: `scripts/verify-static.ps1`
- Linux/container E2E checks: `scripts/e2e-linux.sh`

The current Windows environment can run static builds and scans. Full nsjail,
cgroup v2, Docker build and multi-worker runtime acceptance require Docker
daemon and a Linux host with cgroup v2.
