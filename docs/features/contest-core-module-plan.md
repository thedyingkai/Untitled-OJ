# Contest Core Module Plan

Status: design draft only. This file is not an installer fixture.
Date: 2026-06-27

Contest Core is planned as a Module Contract v1 feature module. No `modules/contest-core/` implementation is created in this planning gate.

## Module Identity

| Field | Draft Value |
| --- | --- |
| `schema_version` | `1` |
| `id` | `ojos.contest-core` |
| `name` | `Contest Core` |
| `version` | `0.1.0` |
| `set` | `contest` |
| `kind` | `feature` |
| `status` | `external` during development, not GA |

## Dependencies

| Dependency | Reason |
| --- | --- |
| `ojos.judge-core` | Submission and judging integration. |
| Problem API platform service | Contest problem set references existing problem ids. |
| Auth/permission registry | Contest admin, participant and viewer permissions. |
| Runtime Snapshot v1 | Module contribution visibility. |

## Draft Manifest Contract

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

## Boundary

The skeleton stage may require a trusted compose entry for `contest-api`. That is a deployment allowlist change, not a manifest-provided arbitrary image. The manifest must not contain `command`, `script`, `hook`, `image`, `mount`, `host_path`, `privileged`, `cap_add`, `target_url`, secrets or token fields.

## Future Submodules

- `ojos.contest-scoreboard`
- `ojos.contest-clarification`
- `ojos.contest-print`
- `ojos.contest-balloon`
- `ojos.contest-team`
- `ojos.contest-rating`
- `ojos.remote-oj`
