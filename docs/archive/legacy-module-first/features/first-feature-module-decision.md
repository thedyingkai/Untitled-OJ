# 第一个业务模块决策

> 文档状态：规划决策，未开始实现
> 最后更新：2026-06-27

## 决策

推荐的第一个真实业务模块是 **Contest Core**，但必须分阶段实现。下一步只能进入 **Contest Core Skeleton**，不能直接实现完整竞赛系统。

## 为什么选择 Contest Core

Contest Core 最适合作为首个业务模块，因为它能用真实产品价值验证当前 Module SDK 和 Runtime 表面：

- 它依赖 Judge Core，但不把 Judge Core 标记为 GA。
- 它需要 permissions、menus、frontend route metadata、gateway routes 和 topology。
- 它可以通过 Module Contract v1 表达。
- 它可以从小型 `contest-api` service boundary 和 metadata-first install flow 开始。
- 它能在添加更多 contest-adjacent modules 前暴露 submissions、scoring 和 participant scope 的设计压力。

## 为什么其他候选不优先

| 候选 | 不作为第一个模块的原因 |
| --- | --- |
| Training | 需要 progress/enrollment model，对 contest-specific Judge Core dependencies 的验证较弱。 |
| Group / Team | 需要更深入的 identity 和 permission inheritance 决策。 |
| Discussion | 有价值，但验证的 OJ-specific runtime constraints 较少。 |
| Clarification | 需要先有 Contest scope。 |
| Print | Operational queue 和安全风险过高，不适合作为第一个模块。 |
| Balloon | 依赖 contest events 和 accepted-submission semantics。 |
| Remote OJ | Trust、credentials 和 external network 风险超出当前范围。 |
| Rating / Ranking | 需要稳定 contest results 和 scheduled computation。 |

## 必需护栏

- 规划门禁期间不写 Contest API、frontend 或 migrations。
- Skeleton 阶段开始前，不提交 `modules/contest-core/module.yaml`。
- 规划门禁不修改 Kernel、Gateway 或 Web Shell core logic。
- 不增加 hooks、remote market support 或 dynamic frontend bundles。
- 不把 Judge Core 标记为 GA。

## 下一阶段建议

只有 planning gate 与 kernel acceptance checks 均通过后，才进入 **Contest Core Skeleton**。Skeleton 应包含：

- Manifest 和 metadata install path。
- 最小 `contest-api` service skeleton。
- Admin/public 占位 route metadata。
- 不包含真实 scoreboard、clarification、print、balloon、rating 或 remote OJ 行为。
