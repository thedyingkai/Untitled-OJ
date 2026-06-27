# Contest Core Data Model Draft

Status: design draft only. No migration is added in this gate.
Date: 2026-06-27

Contest Core owns contest-scoped data. It references Problem API and Judge Core records by stable ids instead of taking ownership of those records.

## Tables

### `contests`

| Field | Meaning |
| --- | --- |
| `id` | Stable contest id. |
| `slug` | Human-readable unique slug. |
| `title` | Display title. |
| `description` | Optional markdown/plain text description. |
| `status` | `draft`, `scheduled`, `running`, `frozen`, `ended`, `archived`, `cancelled`. |
| `starts_at` | Contest start time in UTC. |
| `ends_at` | Contest end time in UTC. |
| `visibility` | `private`, `unlisted`, `public`. |
| `scoreboard_policy` | `placeholder`, `acm_basic`, `ioi_basic` later. |
| `created_by` | User id of creator. |
| `created_at`, `updated_at` | Audit timestamps. |

Indexes and constraints:

- Unique `slug`.
- Index on `status`.
- Index on `starts_at`, `ends_at`.
- Check `ends_at > starts_at`.

### `contest_problems`

| Field | Meaning |
| --- | --- |
| `contest_id` | Contest id. |
| `problem_id` | Problem API problem id. |
| `alias` | Contest label such as `A`. |
| `display_order` | Stable problem order. |
| `points` | Optional points metadata. |
| `visible_from` | Optional contest-local visibility time. |

Indexes and constraints:

- Unique `(contest_id, problem_id)`.
- Unique `(contest_id, alias)`.
- Unique `(contest_id, display_order)`.
- Foreign key to `contests.id`; cross-module `problem_id` is a soft reference validated through Problem API.

### `contest_participants`

| Field | Meaning |
| --- | --- |
| `contest_id` | Contest id. |
| `user_id` | Participant user id. |
| `participant_type` | `official`, `unofficial`, `observer`, `admin`. |
| `status` | `invited`, `registered`, `active`, `removed`, `banned`. |
| `joined_at` | Join timestamp. |

Indexes and constraints:

- Unique `(contest_id, user_id)`.
- Index on `(contest_id, status)`.

### `contest_roles`

| Field | Meaning |
| --- | --- |
| `contest_id` | Contest id. |
| `user_id` | User id. |
| `role` | `owner`, `manager`, `judge`, `viewer`. |

Indexes and constraints:

- Unique `(contest_id, user_id, role)`.
- Index on `(user_id, role)`.

### `contest_submissions`

| Field | Meaning |
| --- | --- |
| `contest_id` | Contest id. |
| `submission_id` | Judge Core submission id. |
| `problem_id` | Problem id at submission time. |
| `user_id` | Submitter user id. |
| `participant_id` | Optional participant row reference. |
| `submitted_at` | UTC timestamp. |
| `status_snapshot` | Judging status snapshot used for contest views. |

Indexes and constraints:

- Unique `(contest_id, submission_id)`.
- Index on `(contest_id, user_id, submitted_at)`.
- Index on `(contest_id, problem_id, submitted_at)`.

### `contest_score_snapshots`

| Field | Meaning |
| --- | --- |
| `contest_id` | Contest id. |
| `snapshot_id` | Snapshot id. |
| `generated_at` | UTC timestamp. |
| `policy` | Score policy used. |
| `payload` | Versioned scoreboard JSON payload. |
| `is_public` | Whether the snapshot can be shown publicly. |

Indexes and constraints:

- Unique `(contest_id, snapshot_id)`.
- Index on `(contest_id, generated_at)`.

### `contest_freeze_windows` Optional Draft

| Field | Meaning |
| --- | --- |
| `contest_id` | Contest id. |
| `starts_at`, `ends_at` | Freeze window in UTC. |
| `policy` | `hide_new_results`, `hide_score_delta`, future values. |

This table is optional for the skeleton and should not be implemented until freeze policy is reviewed.

## State Machine

`draft -> scheduled -> running -> frozen -> ended -> archived`

Alternative terminal transitions:

- `draft -> cancelled`
- `scheduled -> cancelled`
- `running -> ended`
- `ended -> archived`

Only `contest.manage` can change lifecycle state. Time-based transitions should be explicit in v1; scheduled automatic transitions are a later worker/job feature.

## Cross-Module References

- Problem references are soft references to Problem API ids.
- Submission references are soft references to Judge Core submission ids.
- User references use platform identity ids.
- Contest Core should avoid foreign keys into tables owned by other modules until cross-module migration governance exists.

## Migration Ownership

Contest Core will need module-owned migrations when implementation begins. This planning gate does not add real migrations.
