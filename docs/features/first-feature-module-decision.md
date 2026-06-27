# First Feature Module Decision

Status: planning decision, no implementation started.
Date: 2026-06-27

## Decision

The recommended first real business module is **Contest Core**, but only as a staged implementation. The next implementation stage should be **Contest Core Skeleton**, not a complete contest system.

## Why Contest Core

Contest Core is the best fit because it validates the current Module SDK and Runtime surfaces with real product value:

- It depends on Judge Core without making Judge Core GA.
- It needs permissions, menus, frontend route metadata, gateway routes and topology.
- It can be represented through Module Contract v1.
- It can start with a small `contest-api` service boundary and metadata-first install flow.
- It exposes important design pressure around submissions, scoring and participant scope before more contest-adjacent modules are added.

## Why Not The Other Candidates First

| Candidate | Reason Not First |
| --- | --- |
| Training | Needs progress/enrollment model and is less effective at testing contest-specific Judge Core dependencies. |
| Group / Team | Requires deeper identity and permission inheritance decisions. |
| Discussion | Useful but validates fewer OJ-specific runtime constraints. |
| Clarification | Needs Contest scope first. |
| Print | Operational queue and security risk is too high for the first module. |
| Balloon | Depends on contest events and accepted-submission semantics. |
| Remote OJ | Trust, credentials and external network risk are out of scope. |
| Rating / Ranking | Needs stable contest results and scheduled computation. |

## Required Guardrails

- Do not write Contest API, frontend or migrations during this planning gate.
- Do not create a checked-in `modules/contest-core/module.yaml` until the skeleton stage starts.
- Do not modify Kernel, Gateway or Web Shell core logic for the planning gate.
- Do not add hooks, remote market support or dynamic frontend bundles.
- Do not mark Judge Core GA.

## Next Stage Recommendation

Proceed to **Contest Core Skeleton** only if the planning gate and kernel acceptance checks pass. The skeleton should contain:

- Manifest and metadata install path.
- Minimal `contest-api` service skeleton.
- Admin/public placeholder route metadata.
- No real scoreboard, clarification, print, balloon, rating or remote OJ behavior.
