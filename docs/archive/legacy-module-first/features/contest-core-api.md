# Contest Core API 草案

> 文档状态：设计草案，不是已实现 API
> 最后更新：2026-06-27

Contest Core API 未来应通过 Gateway dynamic proxy 暴露在 `/api/contest`，并绑定到 `service_id: contest-api`。当前仓库没有实现 Contest API，本文件只用于下一阶段设计评审。

## 通用规则

- Gateway route prefix: `/api/contest`.
- Service id: `contest-api`.
- Auth mode: `user` for normal contest views and submissions; `admin` only for platform-level diagnostics.
- 所有响应必须使用稳定错误结构，不能泄露 host paths。
- 原始 `Authorization` 默认不转发到内部服务；Gateway internal auth/HMAC 规则仍适用。
- 普通用户缺权限返回 `403`，无 token 返回 `401`。

## Endpoints

### `GET /api/contest/contests`

必需权限：`contest.view`。

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

错误：`401`、`403`、`500`。

### `POST /api/contest/contests`

必需权限：`contest.manage`。

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

响应：创建后的 contest object。

错误：`400` validation、`401`、`403`、`409` slug conflict、`500`。

### `GET /api/contest/contests/:id`

必需权限：`contest.view`，并叠加 contest visibility/participant policy。

响应：contest detail，包含 problem count 与 participant status。

错误：`401`、`403`、`404`、`500`。

### `PATCH /api/contest/contests/:id`

必需权限：`contest.manage`。

请求：局部 contest 字段，例如 `title`、`description`、`starts_at`、`ends_at`、`visibility`、`status`。

错误：`400`、`401`、`403`、`404`、`409`、`500`。

### `POST /api/contest/contests/:id/problems`

必需权限：`contest.manage`。

Request:

```json
{
  "problem_id": "problem_01",
  "alias": "A",
  "display_order": 1,
  "points": 100
}
```

该端点需要通过 Problem API 或受信任本地 problem reference adapter 校验 `problem_id`。

错误：`400`、`401`、`403`、`404`、`409`、`502` problem dependency unavailable。

### `POST /api/contest/contests/:id/participants`

必需权限：代他人添加需要 `contest.manage`；策略允许自注册时需要 `contest.participate`。

Request:

```json
{
  "user_id": "user_01",
  "participant_type": "official"
}
```

错误：`400`、`401`、`403`、`404`、`409`。

### `GET /api/contest/contests/:id/scoreboard`

必需权限：`contest.view`；contest policy 控制 public visibility。

Response:

```json
{
  "contest_id": "contest_01",
  "policy": "placeholder",
  "generated_at": null,
  "rows": []
}
```

Skeleton 阶段可以返回占位 payload；真实 scoreboard computation 不在 skeleton 范围内。

错误：`401`、`403`、`404`，scoreboard 未启用时可返回 `501`。

### `GET /api/contest/contests/:id/submissions`

必需权限：`contest.view`，并叠加 contest participant/admin scope。

查询：`user_id`、`problem_id`、`page`、`page_size`。

响应：contest-scoped submission references 与 status snapshots。

错误：`401`、`403`、`404`、`500`。

## Path Leak 防护

错误响应不能包含 absolute paths、container paths、local compose paths、`.env` values、tokens、worker tokens 或 database URLs。E2E 必须保持 `path_leaks=0`。

## Dynamic Gateway Route

该 route 由 manifest metadata 贡献：

- `prefix: /api/contest`
- `service_id: contest-api`
- `auth_mode: user`
- `required_permission: contest.view`

Gateway 必须通过 trusted service configuration 解析 `contest-api`，不能从 manifest 提供的 `target_url` 解析。
