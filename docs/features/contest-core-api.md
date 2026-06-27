# Contest Core API Draft

Status: design draft only. No Contest API is implemented in this gate.
Date: 2026-06-27

Contest Core API is expected to route through the dynamic Gateway proxy under `/api/contest` and bind to `service_id: contest-api`.

## Common Rules

- Gateway route prefix: `/api/contest`.
- Service id: `contest-api`.
- Auth mode: `user` for normal contest views and submissions; `admin` only for platform-level diagnostics.
- All responses must use stable error payloads and must not leak host paths.
- Raw `Authorization` is not forwarded to internal services by default; Gateway internal auth/HMAC rules apply.
- Ordinary users receive `403` for missing permissions; missing token receives `401`.

## Endpoints

### `GET /api/contest/contests`

Required permission: `contest.view`.

Query:

```json
{
  "status": "scheduled|running|ended|archived",
  "visibility": "public|unlisted",
  "page": 1,
  "page_size": 20
}
```

Response:

```json
{
  "items": [
    {
      "id": "contest_01",
      "slug": "spring-2026",
      "title": "Spring Contest 2026",
      "status": "scheduled",
      "starts_at": "2026-07-01T10:00:00Z",
      "ends_at": "2026-07-01T15:00:00Z"
    }
  ],
  "page": 1,
  "page_size": 20,
  "total": 1
}
```

Errors: `401`, `403`, `500`.

### `POST /api/contest/contests`

Required permission: `contest.manage`.

Request:

```json
{
  "slug": "spring-2026",
  "title": "Spring Contest 2026",
  "description": "Draft contest",
  "starts_at": "2026-07-01T10:00:00Z",
  "ends_at": "2026-07-01T15:00:00Z",
  "visibility": "private"
}
```

Response: created contest object.

Errors: `400` validation, `401`, `403`, `409` slug conflict, `500`.

### `GET /api/contest/contests/:id`

Required permission: `contest.view` plus contest visibility/participant policy.

Response: contest detail including problem count and participant status.

Errors: `401`, `403`, `404`, `500`.

### `PATCH /api/contest/contests/:id`

Required permission: `contest.manage`.

Request: partial contest fields such as `title`, `description`, `starts_at`, `ends_at`, `visibility`, `status`.

Errors: `400`, `401`, `403`, `404`, `409`, `500`.

### `POST /api/contest/contests/:id/problems`

Required permission: `contest.manage`.

Request:

```json
{
  "problem_id": "problem_01",
  "alias": "A",
  "display_order": 1,
  "points": 100
}
```

This endpoint validates `problem_id` through Problem API or a trusted local problem reference adapter.

Errors: `400`, `401`, `403`, `404`, `409`, `502` problem dependency unavailable.

### `POST /api/contest/contests/:id/participants`

Required permission: `contest.manage` for adding others; `contest.participate` for self-registration where policy allows.

Request:

```json
{
  "user_id": "user_01",
  "participant_type": "official"
}
```

Errors: `400`, `401`, `403`, `404`, `409`.

### `GET /api/contest/contests/:id/scoreboard`

Required permission: `contest.view`; contest policy controls public visibility.

Response:

```json
{
  "contest_id": "contest_01",
  "policy": "placeholder",
  "generated_at": null,
  "rows": []
}
```

Skeleton may return a placeholder payload until real scoreboard computation is implemented.

Errors: `401`, `403`, `404`, `501` if scoreboard is not enabled.

### `GET /api/contest/contests/:id/submissions`

Required permission: `contest.view` with contest participant/admin scope.

Query: `user_id`, `problem_id`, `page`, `page_size`.

Response: contest-scoped submission references and status snapshots.

Errors: `401`, `403`, `404`, `500`.

## Path Leak Defense

Errors must never include absolute paths, container paths, local compose paths, `.env` values, tokens, worker tokens or database URLs. E2E must keep `path_leaks=0`.

## Dynamic Gateway Route

The route is contributed by manifest metadata:

- `prefix: /api/contest`
- `service_id: contest-api`
- `auth_mode: user`
- `required_permission: contest.view`

Gateway must resolve `contest-api` through trusted service configuration, never from manifest-provided `target_url`.
