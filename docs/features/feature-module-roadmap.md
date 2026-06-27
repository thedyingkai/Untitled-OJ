# Feature Module Roadmap

Status: planning gate, no business code implemented.
Date: 2026-06-27

This roadmap compares candidate feature modules before OJOS starts the first real business module. It assumes Kernel Baseline Freeze is complete at commit `3baa0e3e1cf5605731430eea1a79d18dd85b37c0`.

## Decision Criteria

Each candidate is evaluated by business value, dependency shape, use of the current Module SDK and Runtime, required Kernel changes, and implementation risk.

| Candidate | Business Value | Depends On Judge Core | Runtime Snapshot | Dynamic Route | Service Runtime | New Extension Point | Kernel Change Risk | First Module Fit | Risk | Priority |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Contest Core | Very high | Yes | Yes | Yes | Likely | No for skeleton | Medium | Strong | High | P0 |
| Training | High | Yes | Yes | Yes | Maybe | Maybe, progress model | Medium | Good after Contest | Medium | P1 |
| Group / Team | High | Indirect | Yes | Yes | Maybe | Maybe, membership model | Medium | Useful but auth-sensitive | Medium | P1 |
| Discussion | Medium | No | Yes | Yes | Yes | No | Low | Good simple module | Medium | P2 |
| Clarification | High for contests | Yes | Yes | Yes | Maybe | No | Low | Should follow Contest | Medium | P1 after Contest |
| Print | Medium | No | Yes | Yes | Yes/worker | Maybe, queue policy | Medium | Too operational first | High | P3 |
| Balloon | Medium | Yes | Yes | Yes | Worker | Maybe, event hooks later | Medium | Should follow Contest | Medium | P3 |
| Remote OJ | High | No/optional | Yes | Yes | Yes | Trust/integration points | High | Not first | Very high | P4 |
| Rating / Ranking | Medium | Yes | Yes | Yes | Worker/jobs | Maybe, scheduled ranking | Medium | Should follow contests | High | P3 |

## Candidate Notes

### Contest Core

Contest Core is the strongest first real business module because it exercises permissions, routes, menus, topology, services, storage metadata, events and Judge Core dependency boundaries. It must be split into a conservative skeleton first; full XCPC/IOI behavior, rolling scoreboard, clarification, print and balloon are separate follow-on modules.

### Training

Training is valuable and simpler than full contest operations, but it tends to introduce progress tracking, enrollment and curriculum concepts. It is a good second or parallel feature after Contest Core proves the module path.

### Group / Team

Group and team features are foundational for contests and training, but they touch identity, membership and permission inheritance. They are important, yet they can block on deeper access-control design.

### Discussion

Discussion is a good route/menu/API module candidate and can be kept mostly independent from Judge Core. It is not as strong as a first module because it validates fewer OJ-specific integration points.

### Clarification

Clarification is best modeled as a Contest submodule or companion module. Starting with it before Contest Core would force fake contest scope semantics.

### Print

Print requires operational policies, queues and potentially worker-side output handling. It should wait until Contest Core and controlled worker patterns are stable.

### Balloon

Balloon depends on contest submission events and accepted-status transitions. It should not be first because the event contract is not frozen enough for operational side effects.

### Remote OJ

Remote OJ has trust, network, credential, rate-limit and external dependency risk. It is explicitly not a first real module.

### Rating / Ranking

Rating needs contest results, scheduled computation, data retention and anti-abuse policy. It should follow real contest data rather than define the first module.

## Recommended Order

1. Contest Core Skeleton.
2. Contest Core Minimal v1.
3. Clarification or Group/Team depending on product priority.
4. Training.
5. Scoreboard advanced module.
6. Print / Balloon.
7. Rating / Ranking.
8. Remote OJ after a separate trust and integration review.

## Kernel Evolution Watchlist

The current Module Contract v1 can host the Contest Core skeleton. Kernel evolution may be needed later for:

- Dynamic frontend bundle loading with a secure L3 design.
- New event delivery semantics for side-effecting modules such as balloon and rating.
- New runtime drivers beyond trusted compose.
- Package signature and trust policy.
- Multi-machine controlled apply.
