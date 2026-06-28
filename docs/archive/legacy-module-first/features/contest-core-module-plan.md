# Contest Core 模块计划

> 文档状态：设计草案，不是 installer fixture
> 最后更新：2026-06-27

Contest Core 计划作为 Module Contract v1 feature module 接入。当前仓库没有创建 `modules/contest-core/` 实现。

## 模块身份

| 字段 | 草案值 |
| --- | --- |
| `schema_version` | `1` |
| `id` | `ojos.contest-core` |
| `name` | `Contest Core` |
| `version` | `0.1.0` |
| `set` | `contest` |
| `kind` | `feature` |
| `status` | 开发期为 `external`，不标记通用可用状态 |

## 依赖

| 依赖 | 原因 |
| --- | --- |
| `ojos.judge-core` | 提交与评测集成。 |
| Problem API platform service | Contest problem set 引用已有 problem ids。 |
| Auth/permission registry | Contest admin、participant 和 viewer 权限。 |
| Runtime Snapshot v1 | 模块贡献可见性。 |

## Manifest 契约草案

```yaml
schema_version: 1
id: ojos.contest-core
name: Contest Core
version: 0.1.0
set: contest
kind: feature
status: external
description: Minimal contest entity, participation and problem binding module.
compatibility:
  platform: ">=0.1.0"
  installer: ">=0.1.0"
requires:
  modules:
    - id: ojos.judge-core
      version: ">=0.1.0"
provides:
  permissions:
    - key: contest.view
      name: View contests
    - key: contest.participate
      name: Participate in contests
    - key: contest.manage
      name: Manage contests
  roles:
    - id: contest-admin
      name: Contest Admin
      permissions:
        - contest.view
        - contest.participate
        - contest.manage
  components:
    - id: contest-shell
      name: Contest Shell Metadata
      kind: frontend-placeholder
  frontend_routes:
    - path: /contests
      component: contest.list
      required_permission: contest.view
    - path: /contests/:id
      component: contest.detail
      required_permission: contest.view
    - path: /admin/contests
      component: contest.admin
      required_permission: contest.manage
  menus:
    - id: contests
      label: Contests
      path: /contests
      required_permission: contest.view
      enabled: true
    - id: admin-contests
      label: Contests
      path: /admin/contests
      required_permission: contest.manage
      enabled: true
  services:
    - id: contest-api
      name: Contest API
      kind: http
      lifecycle: managed
      trusted_runtime: compose
      compose_service: contest-api
      health_check_id: contest-api-health
      routes:
        - /api/contest
      required: true
  workers:
    - id: contest-scoreboard-worker
      name: Contest Scoreboard Worker
      kind: worker
      lifecycle: metadata
      trusted_runtime: metadata
      required: false
  gateway_routes:
    - id: contest-api
      prefix: /api/contest
      service_id: contest-api
      auth_mode: user
      required_permission: contest.view
      enabled: true
  health_checks:
    - id: contest-api-health
      name: Contest API health
      kind: http
      service_id: contest-api
      path: /healthz
      required: true
  storage_buckets:
    - id: contest-exports
      name: Contest exports
      kind: metadata
  events:
    publishes:
      - contest.created
      - contest.updated
      - contest.participant.joined
    subscribes:
      - judge.submission.completed
  scheduled_jobs:
    - id: contest-scoreboard-refresh
      name: Scoreboard refresh placeholder
      lifecycle: metadata
      enabled: false
  admin_panels:
    - id: contest-admin
      name: Contest Admin
      route: /admin/contests
      required_permission: contest.manage
  topology:
    nodes:
      - id: module:ojos.contest-core
        kind: module
        label: Contest Core
      - id: service:contest-api
        kind: service
        label: Contest API
      - id: worker:contest-scoreboard-worker
        kind: worker
        label: Contest Scoreboard Worker
    edges:
      - from: module:ojos.contest-core
        to: service:contest-api
        kind: provides
      - from: service:contest-api
        to: route:/api/contest
        kind: route
      - from: service:contest-api
        to: health:contest-api-health
        kind: health
      - from: module:ojos.contest-core
        to: module:ojos.judge-core
        kind: depends_on
package:
  format: ojosmod
  version: 1
```

## 边界

Skeleton 阶段可能需要为 `contest-api` 增加 trusted compose entry。该变更属于 deployment allowlist，不是 manifest 提供 arbitrary image。Manifest 不得包含 `command`、`script`、`hook`、`image`、`mount`、`host_path`、`privileged`、`cap_add`、`target_url`、secret 或 token 字段。

## 后续子模块

- `ojos.contest-scoreboard`
- `ojos.contest-clarification`
- `ojos.contest-print`
- `ojos.contest-balloon`
- `ojos.contest-team`
- `ojos.contest-rating`
- `ojos.remote-oj`
