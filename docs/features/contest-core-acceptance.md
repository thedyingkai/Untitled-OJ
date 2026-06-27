# Contest Core Acceptance Matrix

Status: design draft only. No Contest acceptance script is added in this gate.
Date: 2026-06-27

This matrix defines the checks required before a future Contest Core Skeleton can be accepted.

| Area | Check | Expected Result |
| --- | --- | --- |
| Manifest | `ojosctl module validate modules/contest-core` | Schema v1 accepted; no dangerous fields. |
| Package | `ojosctl module package modules/contest-core` | `.ojosmod` contains manifest, checksums and package metadata. |
| Verify | `ojosctl module verify <contest-core.ojosmod>` | Checksum verification ok. |
| Install dry-run | Installer dry-run | Safe plan, no DB writes. |
| Install apply | Installer apply | Registry entry and contributions written. |
| Enable | Enable module | Active snapshot includes Contest Core contributions. |
| Disable | Disable module | Active routes, menus and permissions removed. |
| Runtime snapshot | Admin runtime snapshot | `ojos.contest-core` appears when enabled. |
| Runtime routes | Route table | `/api/contest` bound to `contest-api` or disabled metadata route. |
| Runtime services | Runtime services | `contest-api` state visible; metadata workers visible. |
| Topology | Topology API/UI | Contest module, service, route, health, Judge Core dependency edges visible. |
| Permissions | Permission registry | `contest.view`, `contest.participate`, `contest.manage` visible only when active. |
| Frontend | Web Shell contribution registry | Menu/routes appear without hardcoded Contest menu logic. |
| API e2e | Contest API smoke | `401` no token, `403` missing permission, no path leaks. |
| Path leaks | e2e path scan | `path_leaks=0`. |
| Judge Core | Compatibility | Judge Core remains enabled/protected and not GA. |
| Module compat | Existing harness | sample-hello and demo/judge-core compatibility still pass. |

## Required Regression Commands

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN
powershell -NoProfile -File scripts\e2e-module-compat.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123
```

## Skeleton Acceptance Minimum

The future skeleton is acceptable only if it proves module install/enable/disable, contribution visibility and API permission boundaries without implementing full scoreboard, clarification, print, balloon, rating or remote OJ.
