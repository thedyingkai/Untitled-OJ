# Contest Core Risk Review

Status: planning risk review, no implementation started.
Date: 2026-06-27

## Risk Summary

| Risk | Severity | Blocking For Skeleton | Mitigation |
| --- | --- | --- | --- |
| Permission model bypass | High | Yes | Define `contest.view`, `contest.participate`, `contest.manage`; enforce in Gateway and service. |
| Freeze/scoreboard semantics | High | No | Keep scoreboard placeholder; defer real freeze. |
| Submission ownership | High | Yes | Bind contest submissions by user, problem and contest time window. |
| Scoreboard consistency | High | No | Snapshot placeholder first; worker later. |
| High-concurrency submissions | Medium | No | Skeleton reads existing submissions only or stores minimal binding. |
| Judge Core dependency drift | Medium | Yes | Keep Judge Core dependency explicit and avoid marking GA. |
| Cross-module data references | Medium | Yes | Use soft references and service validation; avoid cross-module FKs. |
| Dynamic proxy exposure | Medium | Yes | Use `service_id` and trusted route table only. |
| Frontend permission bypass | Medium | Yes | Treat frontend guard as UX only; API remains authoritative. |
| Time and timezone errors | High | Yes | Store UTC; define lifecycle transitions clearly. |
| Cheating/rejudge/upsolve boundaries | Medium | No | Defer advanced policy; document non-goals. |

## Blocking Items

- Contest permission keys and API authorization rules must be frozen before coding.
- Contest state machine must be agreed before migrations.
- Problem and submission reference strategy must avoid cross-module ownership violations.
- Route must use `service_id: contest-api`; no manifest `target_url`.

## Non-Blocking Items

- Full scoreboard computation.
- Freeze windows.
- Clarification.
- Print.
- Balloon.
- Rating.
- Remote OJ integration.
- Advanced anti-cheat policy.

## Kernel Evolution Items

- Secure dynamic frontend bundles for rich Contest UI.
- Event delivery semantics for side-effecting modules.
- Package signature and trust policy.
- Multi-machine runtime apply.
- Runtime drivers beyond trusted compose.

## Module-Internal Items

- Contest CRUD validation.
- Participant registration rules.
- Contest problem aliasing.
- Contest-scoped submission listing.
- Placeholder scoreboard response.
- Admin panel placeholder and route metadata.
