# Contest Core Pre-Implementation Gate

Status: planning gate.
Date: 2026-06-27

## Gate Result

Contest Core is recommended as the first real business module, but the next stage should implement only **Contest Core Skeleton**.

## Recommended First Implementation Scope

- `modules/contest-core/` manifest using Module Contract v1.
- Metadata install, package, verify, enable and disable path.
- Minimal `contest-api` service skeleton if deployment allowlist work is accepted.
- Admin/public placeholder route metadata.
- Permission keys: `contest.view`, `contest.participate`, `contest.manage`.
- Runtime snapshot, route table, services and topology visibility.
- API smoke only: health and placeholder endpoints with correct `401`/`403` behavior.

## Explicitly Out Of Scope

- Complete Contest API.
- Complete Contest frontend.
- Real scoreboard.
- Rolling scoreboard.
- Complex freeze windows.
- Clarification.
- Print.
- Balloon.
- Team management.
- Remote OJ.
- Rating.
- Advanced anti-cheat.
- Judge Core GA.

## Kernel Prerequisites

No Kernel core change is required for the skeleton if it stays inside Module Contract v1. A trusted compose service allowlist update may be required for `contest-api`; that is not a manifest escape hatch.

## Required Acceptance Commands

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN
powershell -NoProfile -File scripts\e2e-module-compat.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123
```

## Rollback Strategy

- Disable `ojos.contest-core` to remove active menus, permissions and routes.
- Stop or remove trusted `contest-api` compose service if it was introduced.
- Keep module registry history and audit entries.
- Do not delete Judge Core or shared problem/submission data.

## Final Decision

The planning gate is sufficient to start the skeleton stage after regression checks pass. It is not sufficient to claim Contest is implemented.
