# Contest Core Frontend Contribution Draft

Status: design draft only. No Contest frontend is implemented in this gate.
Date: 2026-06-27

Contest Core must use the current Web Shell contribution registry. It must not require dynamic frontend bundle loading in the skeleton stage.

## Menu Contributions

| Menu | Path | Permission | Notes |
| --- | --- | --- | --- |
| Contests | `/contests` | `contest.view` | Public/user-facing contest list. |
| Admin Contests | `/admin/contests` | `contest.manage` | Admin panel contribution. |

Menu entries must disappear or become disabled when the module is disabled.

## Frontend Routes

| Route | Component Key | Permission | Skeleton Behavior |
| --- | --- | --- | --- |
| `/contests` | `contest.list` | `contest.view` | Shell fallback or placeholder contribution. |
| `/contests/:id` | `contest.detail` | `contest.view` | Shell fallback until component exists. |
| `/contests/:id/problems` | `contest.problem_list` | `contest.view` | Placeholder route. |
| `/contests/:id/submissions` | `contest.submissions` | `contest.view` | Placeholder route. |
| `/contests/:id/scoreboard` | `contest.scoreboard` | `contest.view` | Placeholder, no real scoreboard. |
| `/admin/contests` | `contest.admin` | `contest.manage` | Admin placeholder. |

## Required Shell Behavior

- Unknown component keys use safe fallback UI.
- Permission guard is enforced by Web Shell metadata and API authorization.
- No sample-specific or Contest-specific hardcoding is added to the main menu, topology page or permission page.
- No dynamic untrusted JavaScript is loaded.

## Future L3 Boundary

A richer Contest UI may need secure dynamic frontend bundles. That belongs to L3 Dynamic Frontend Extension and is not part of Contest Core Skeleton.
