# Contest Core 风险评审

> 文档状态：规划风险评审，未开始实现
> 最后更新：2026-06-27

## 风险摘要

| 风险 | 严重度 | 是否阻塞 Skeleton | 缓解措施 |
| --- | --- | --- | --- |
| Permission model bypass | 高 | 是 | 定义 `contest.view`、`contest.participate`、`contest.manage`，并在 Gateway 与 service 中强制校验。 |
| Freeze/scoreboard semantics | 高 | 否 | Skeleton 保留 scoreboard 占位；真实封榜延后。 |
| Submission ownership | 高 | 是 | 通过用户、题目和 contest time window 绑定 contest submissions。 |
| Scoreboard consistency | 高 | 否 | 先做 snapshot 占位，后续再引入 worker。 |
| High-concurrency submissions | 中 | 否 | Skeleton 只读取已有 submissions 或保存最小 binding。 |
| Judge Core dependency drift | 中 | 是 | 明确 Judge Core dependency，且不标记通用可用状态。 |
| Cross-module data references | 中 | 是 | 使用 soft references 和 service validation，避免 cross-module FKs。 |
| Dynamic proxy exposure | 中 | 是 | 只使用 `service_id` 和 trusted route table。 |
| Frontend permission bypass | 中 | 是 | 前端 guard 只作为 UX；API 保持权威校验。 |
| Time and timezone errors | 高 | 是 | 存储 UTC，并清晰定义 lifecycle transitions。 |
| Cheating/rejudge/upsolve boundaries | 中 | 否 | 延后高级策略，并写清非目标。 |

## 阻塞项

- 编码前必须冻结 Contest permission keys 和 API authorization rules。
- 写 migration 前必须确认 Contest state machine。
- Problem 与 submission reference strategy 必须避免 cross-module ownership violations。
- Route 必须使用 `service_id: contest-api`，不得使用 manifest `target_url`。

## 非阻塞项

- 完整 scoreboard computation。
- Freeze windows。
- Clarification.
- Print.
- Balloon.
- Rating.
- Remote OJ integration.
- 高级 anti-cheat policy。

## 需要 Kernel 演进的项

- Rich Contest UI 需要 secure dynamic frontend bundles。
- 有副作用模块需要 event delivery semantics。
- Package signature 和 trust policy。
- Multi-machine runtime apply。
- Trusted compose 之外的 runtime drivers。

## 模块内部可解决项

- Contest CRUD validation。
- Participant registration rules。
- Contest problem aliasing。
- Contest-scoped submission listing。
- Scoreboard 占位响应。
- Admin panel 占位与 route metadata。
