# 业务模块路线图

> 文档状态：规划门禁，未实现业务代码
> 最后更新：2026-06-27

本文在 OJOS 开始第一个真实业务模块前比较候选 feature modules。本文假设 Kernel Baseline Freeze 已完成于 commit `3baa0e3e1cf5605731430eea1a79d18dd85b37c0`。

## 决策标准

每个候选模块按业务价值、依赖形态、是否使用当前 Module SDK 与 Runtime、是否需要 Kernel 变更、实现风险进行评估。

| 候选 | 业务价值 | 依赖 Judge Core | Runtime Snapshot | Dynamic Route | Service Runtime | 新 extension point | Kernel 变更风险 | 适合作为首个模块 | 风险 | 优先级 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Contest Core | 很高 | 是 | 是 | 是 | 可能 | Skeleton 不需要 | 中 | 强 | 高 | P0 |
| Training | 高 | 是 | 是 | 是 | 可能 | 可能需要 progress model | 中 | Contest 之后较合适 | 中 | P1 |
| Group / Team | 高 | 间接 | 是 | 是 | 可能 | 可能需要 membership model | 中 | 有价值但权限敏感 | 中 | P1 |
| Discussion | 中 | 否 | 是 | 是 | 是 | 否 | 低 | 可作为简单模块 | 中 | P2 |
| Clarification | 对竞赛很高 | 是 | 是 | 是 | 可能 | 否 | 低 | 应跟随 Contest | 中 | Contest 后 P1 |
| Print | 中 | 否 | 是 | 是 | 是/worker | 可能需要 queue policy | 中 | 首个模块过于偏运维 | 高 | P3 |
| Balloon | 中 | 是 | 是 | 是 | Worker | 后续可能需要 event hooks | 中 | 应跟随 Contest | 中 | P3 |
| Remote OJ | 高 | 否/可选 | 是 | 是 | 是 | trust/integration points | 高 | 不适合首个模块 | 很高 | P4 |
| Rating / Ranking | 中 | 是 | 是 | 是 | Worker/jobs | 可能需要 scheduled ranking | 中 | 应跟随 contest results | 高 | P3 |

## 候选说明

### Contest Core

Contest Core 是最强的第一个真实业务模块候选，因为它会验证 permissions、routes、menus、topology、services、storage metadata、events 和 Judge Core dependency boundaries。它必须先拆成保守 skeleton；完整 XCPC/IOI 行为、滚榜、clarification、print 和 balloon 是后续独立模块。

### Training

Training 有价值，也比完整 contest operations 简单，但会引入 progress tracking、enrollment 和 curriculum 概念。Contest Core 证明模块路径后，它适合作为第二个或并行 feature。

### Group / Team

Group 和 team 对 contests/training 很重要，但会触及 identity、membership 和 permission inheritance。它们很重要，但可能被更深入的 access-control design 阻塞。

### Discussion

Discussion 是不错的 route/menu/API module 候选，也基本可以独立于 Judge Core。它不如 Contest Core 适合作为第一个模块，因为验证的 OJ-specific integration points 较少。

### Clarification

Clarification 更适合作为 Contest submodule 或 companion module。先做它会迫使系统伪造 contest scope semantics。

### Print

Print 需要 operational policies、queues，并可能涉及 worker-side output handling。它应等待 Contest Core 和 controlled worker patterns 稳定。

### Balloon

Balloon 依赖 contest submission events 和 accepted-status transitions。当前 event contract 对有副作用的运维流程还不够稳定，因此不适合作为第一个模块。

### Remote OJ

Remote OJ 有 trust、network、credential、rate-limit 和 external dependency 风险，明确不作为第一个真实模块。

### Rating / Ranking

Rating 需要 contest results、scheduled computation、data retention 和 anti-abuse policy。它应基于真实 contest data，而不是定义第一个模块。

## 推荐顺序

1. Contest Core Skeleton.
2. Contest Core Minimal v1.
3. Clarification or Group/Team depending on product priority.
4. Training.
5. Scoreboard advanced module.
6. Print / Balloon.
7. Rating / Ranking.
8. Remote OJ after a separate trust and integration review.

## Kernel 演进观察项

当前 Module Contract v1 可以承载 Contest Core skeleton。后续场景可能需要 Kernel 演进：

- Dynamic frontend bundle loading with a secure L3 design.
- New event delivery semantics for side-effecting modules such as balloon and rating.
- New runtime drivers beyond trusted compose.
- Package signature and trust policy.
- Multi-machine controlled apply.
