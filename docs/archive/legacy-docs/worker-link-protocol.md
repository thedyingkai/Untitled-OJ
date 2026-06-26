> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# Worker Link Protocol

Worker Link decouples judge-worker nodes from the Control Plane. Workers do not
connect to PostgreSQL or Redis and do not read Control Plane local paths.

## Authentication

Every worker request must include:

```http
X-OJOS-Worker-Token: <configured token>
```

Gateway forwards worker paths without user JWT auth but still signs upstream
requests with the internal HMAC scheme. Judge API verifies the worker token.

## Registration

```http
POST /api/judge/worker/register
```

Request:

```json
{
  "worker_id": "worker-node-01",
  "worker_name": "Worker Node 01",
  "hostname": "host-a",
  "version": "0.1.0",
  "capabilities": ["nsjail", "cgroup-v2"],
  "supported_languages": ["cpp17", "c11", "python3", "java17"],
  "max_concurrency": 2
}
```

Response includes heartbeat interval and lease TTL.

## Claim

```http
POST /api/judge/worker/tasks/claim
```

Judge API performs an atomic PostgreSQL claim:

- only `PENDING` submissions can be claimed;
- task status becomes `RUNNING`;
- submission status becomes `JUDGING`;
- `worker_id`, `lease_version`, `lease_expires_at`, `heartbeat_at` and
  `attempt` are updated in one transaction;
- `FOR UPDATE SKIP LOCKED` prevents duplicate claim.

The response includes artifact URLs:

```json
{
  "tasks": [
    {
      "task_id": "sub-42",
      "submission_id": 42,
      "problem_id": 2,
      "language": "cpp17",
      "attempt": 1,
      "lease_version": 1,
      "lease_expires_at": "2026-06-26T05:00:00Z",
      "source": {
        "url": "/judge/worker/artifacts/submissions/42/source?...",
        "sha256": "...",
        "size_bytes": 120,
        "content_type": "text/plain; charset=utf-8"
      },
      "problem_package": {
        "url": "/judge/worker/artifacts/problems/2/package?...",
        "sha256": "...",
        "size_bytes": 4096,
        "content_type": "application/zip"
      }
    }
  ]
}
```

## Lease Heartbeat

```http
POST /api/judge/worker/tasks/:task_id/heartbeat
```

Only the current `worker_id` and `lease_version` can refresh the lease. Expired
leases are rejected.

## Result Upload

```http
POST /api/judge/worker/tasks/:task_id/result
```

The request carries the final summary and per-case summaries/log snippets. Judge
API writes `result.json`, stores bounded logs, and updates the submission
summary. Duplicate upload for the same completed task is ignored by lease
status; stale lease upload is rejected.

## Fail

```http
POST /api/judge/worker/tasks/:task_id/fail
```

Retryable system errors return the task to `PENDING`. User errors finish the
submission as a terminal status.

## Stale Recovery

Before every claim, Judge API recovers expired `RUNNING` tasks:

- task status becomes `PENDING`;
- `worker_id` and lease fields are cleared;
- submission status returns to `PENDING`;
- a later claim increments `lease_version`.

An old worker result cannot overwrite the recovered/new lease because
`worker_id` and `lease_version` must match.

## Redis Stream

Redis Streams remain an internal queue signal:

- stream: `ojos:judge:submissions`;
- diagnostic group name exposed by admin API: `judge-workers`;
- PostgreSQL is the fact source;
- worker nodes do not connect to Redis.

Signals are published by Judge API with approximate trimming:

```text
XADD ojos:judge:submissions MAXLEN ~ 10000
```

Admin queue API exposes stream length and diagnostic pending count. Claim is
DB-driven, so duplicate Redis messages do not duplicate judging. Because remote
workers no longer consume Redis directly, Redis pending entries are not task
leases and are not used for task recovery. `XAUTOCLAIM`/`XACK` are therefore
not part of the Worker Link ownership path; stale work is recovered from
`judge_tasks.lease_expires_at`.
