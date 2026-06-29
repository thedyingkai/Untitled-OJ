# Contest Core 数据模型草案

> 文档状态：设计草案，不是已实现 migration
> 最后更新：2026-06-27

Contest Core 未来拥有 contest-scoped data。它通过 stable ids 引用 Problem API 和 Judge Core records，不接管这些记录的所有权。

## 表设计

### `contests`

| 字段 | 含义 |
| --- | --- |
| `id` | 稳定 contest id。 |
| `slug` | 面向人类的唯一 slug。 |
| `title` | 展示标题。 |
| `description` | 可选 markdown/plain text 描述。 |
| `status` | `draft`、`scheduled`、`running`、`frozen`、`ended`、`archived`、`cancelled`。 |
| `starts_at` | UTC 比赛开始时间。 |
| `ends_at` | UTC 比赛结束时间。 |
| `visibility` | `private`、`unlisted`、`public`。 |
| `scoreboard_policy` | skeleton 阶段为 `placeholder`，后续可扩展 `acm_basic`、`ioi_basic`。 |
| `created_by` | 创建者 user id。 |
| `created_at`, `updated_at` | 审计时间戳。 |

索引与约束：

- Unique `slug`.
- Index on `status`.
- Index on `starts_at`, `ends_at`.
- 检查 `ends_at > starts_at`。

### `contest_problems`

| 字段 | 含义 |
| --- | --- |
| `contest_id` | Contest id。 |
| `problem_id` | Problem API problem id。 |
| `alias` | Contest label，例如 `A`。 |
| `display_order` | 稳定题目顺序。 |
| `points` | 可选 points metadata。 |
| `visible_from` | 可选 contest-local visibility time。 |

索引与约束：

- Unique `(contest_id, problem_id)`.
- Unique `(contest_id, alias)`.
- Unique `(contest_id, display_order)`.
- 外键指向 `contests.id`；跨模块 `problem_id` 是 soft reference，需要通过 Problem API 校验。

### `contest_participants`

| 字段 | 含义 |
| --- | --- |
| `contest_id` | Contest id。 |
| `user_id` | 参赛用户 id。 |
| `participant_type` | `official`、`unofficial`、`observer`、`admin`。 |
| `status` | `invited`、`registered`、`active`、`removed`、`banned`。 |
| `joined_at` | 加入时间。 |

索引与约束：

- Unique `(contest_id, user_id)`.
- Index on `(contest_id, status)`.

### `contest_roles`

| 字段 | 含义 |
| --- | --- |
| `contest_id` | Contest id。 |
| `user_id` | User id。 |
| `role` | `owner`、`manager`、`judge`、`viewer`。 |

索引与约束：

- Unique `(contest_id, user_id, role)`.
- Index on `(user_id, role)`.

### `contest_submissions`

| 字段 | 含义 |
| --- | --- |
| `contest_id` | Contest id。 |
| `submission_id` | Judge Core submission id。 |
| `problem_id` | 提交时的 problem id。 |
| `user_id` | 提交者 user id。 |
| `participant_id` | 可选 participant row reference。 |
| `submitted_at` | UTC 时间戳。 |
| `status_snapshot` | contest view 使用的 judging status snapshot。 |

索引与约束：

- Unique `(contest_id, submission_id)`.
- Index on `(contest_id, user_id, submitted_at)`.
- Index on `(contest_id, problem_id, submitted_at)`.

### `contest_score_snapshots`

| 字段 | 含义 |
| --- | --- |
| `contest_id` | Contest id。 |
| `snapshot_id` | Snapshot id。 |
| `generated_at` | UTC 时间戳。 |
| `policy` | 使用的 score policy。 |
| `payload` | 版本化 scoreboard JSON payload。 |
| `is_public` | snapshot 是否可以公开展示。 |

索引与约束：

- Unique `(contest_id, snapshot_id)`.
- Index on `(contest_id, generated_at)`.

### `contest_freeze_windows` 可选草案

| 字段 | 含义 |
| --- | --- |
| `contest_id` | Contest id。 |
| `starts_at`, `ends_at` | UTC 封榜窗口。 |
| `policy` | `hide_new_results`、`hide_score_delta` 或后续值。 |

该表对 skeleton 可选；封榜策略评审完成前不应实现。

## 状态机

`draft -> scheduled -> running -> frozen -> ended -> archived`

可选终止转换：

- `draft -> cancelled`
- `scheduled -> cancelled`
- `running -> ended`
- `ended -> archived`

只有 `contest.manage` 可以变更 lifecycle state。v1 中基于时间的转换应保持显式操作；scheduled automatic transitions 属于后续 worker/job 能力。

## 跨模块引用

- Problem references 是指向 Problem API ids 的 soft references。
- Submission references 是指向 Judge Core submission ids 的 soft references。
- User references 使用 platform identity ids。
- 在 cross-module migration governance 完成前，Contest Core 应避免对其他模块拥有的表建立外键。

## Migration 所有权

Contest Core 开始实现时需要 module-owned migrations。当前 planning gate 不新增真实 migration。
