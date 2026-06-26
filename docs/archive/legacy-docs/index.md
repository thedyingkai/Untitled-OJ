> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# OJOS Current Documentation Entry

This file used to contain earlier design notes. The deployable architecture is now documented in the current engineering documents below.

Use these files as the source of truth:

- `docs/CURRENT_STATE.md`
- `docs/worker-link-protocol.md`
- `docs/deploy-control-plane.md`
- `docs/deploy-worker-node.md`
- `docs/judge-resource-limits.md`
- `docs/e2e-engineering-acceptance.md`
- `docs/problem/package-format.md`
- `docs/permission/overview.md`

Current key points:

- Gateway is the public API entrypoint.
- Auth, Problem API and Judge API stay internal behind Gateway.
- PostgreSQL is the task and permission fact source.
- Redis Streams are bounded submission signal history, not worker task ownership.
- Judge workers use Worker Link over HTTP(S), authenticate with a worker token, and do not connect to PostgreSQL or Redis.
- Workers download source and problem packages through artifact APIs and verify sha256 digests.
- Resource limits require Linux cgroup v2 and nsjail.
- Public API schema and frontend must not expose internal storage paths.

Run current verification:

```powershell
powershell -NoProfile -File scripts/verify-static.ps1 -SkipDockerBuild
```

Run full Linux/container acceptance on a cgroup v2 host with Docker daemon:

```bash
bash scripts/e2e-linux.sh
```
