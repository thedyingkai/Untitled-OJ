# 模块契约

> 文档状态：当前实现
> 最后更新：2026-06-28

模块与 OJOS 主系统只通过明确 contract 交互：

- `module.yaml` manifest schema。
- `.ojosmod` package format。
- PostgreSQL module registry tables。
- Gateway Admin API。
- Runtime Snapshot。
- Frontend route/menu metadata。
- Operation history 和 audit log。

Installer Core 不依赖 Go 代码，不依赖 frontend 代码。Gateway 不直接读写本地模块文件，而是调用 internal Rust service 或读取 Runtime Snapshot。

## 可声明能力

模块可以声明：

- permissions
- roles
- components
- services
- workers
- frontend_routes
- menus
- gateway_routes
- storage_buckets
- health_checks
- migrations
- events
- scheduled_jobs
- admin_panels
- topology nodes/edges
- dependencies

未声明的能力不得由 installer 隐式公开。

## 安全契约

- 模块不能携带生产 secret。
- 模块不能绕过 Gateway、JWT、`system.admin` 权限和 internal auth。
- 模块包不能包含 `.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target`。
- v0.1.0 不执行模块中的 script 或 hook。
- v0.1.0 不支持 remote module market。
- Web Shell 不动态执行模块 JS。

## Runtime Snapshot 接入

`schema_version: 1` module contract 已被 Runtime Snapshot 消费：

- `permissions` -> active permission registry
- `menus` -> Web Shell menu contribution candidates
- `frontend_routes` -> contribution viewer metadata
- `gateway_routes` -> runtime route table 和 conflict validation
- `services` / `workers` / `components` -> runtime components 和 topology nodes
- `storage_buckets` -> runtime storage metadata
- `health_checks` -> admin health metadata
- `events`、`scheduled_jobs`、`admin_panels` -> manifest-derived runtime metadata
- `topology.nodes` / `topology.edges` -> runtime topology graph

Active Runtime Snapshot 只包含 enabled module。Disabled module 只通过 registry detail 和 `include_disabled=true` 管理视图检查。

## Gateway Route Contract

`gateway_routes` 使用 `service_id` 引用 Gateway trusted service：

```yaml
gateway_routes:
  - prefix: /api/problem
    service_id: problem-api
    auth_mode: user
    enabled: true
```

Manifest 不得提供 `target_url`、public URL、localhost target、Docker socket path、raw port、credential 或 proxy command。

Gateway 通过 trusted service map 解析 `service_id`。Runtime Route Table 阻断 unknown service、duplicate prefix 和 reserved prefix claim。

Reserved prefixes：

```text
/api/auth
/api/admin/modules
/api/admin/health
/api/health
/api/internal
/api/judge/worker
```

## Service / Worker Contract

`schema_version: 1` 支持结构化 service/worker declaration：

```yaml
provides:
  services:
    - id: problem-api
      name: Problem API
      kind: http
      lifecycle: managed
      trusted_runtime: compose
      compose_service: problem-api
      health_check_id: problem-api-health
      routes:
        - /api/problem
      required: true
```

允许的 lifecycle：

```text
managed
metadata
external
manual
```

L2 foundation 只为 trusted allowlisted `managed` service 生成 compose plan。`metadata` service 进入 snapshot/topology，但不能 start、stop 或 restart。

Manifest 不能指定 `command`、`script`、`image`、`mount`、`host_path`、`privileged` 或 `cap_add` 等可执行 runtime control fields。

## Controlled Apply Contract

Manifest 描述服务意图，不授予执行权。Runtime apply 由 trusted Kernel/operator policy 派生，不由 manifest executable field 派生。

Controlled apply 使用 runtime plan：

```text
commands[].argv
allowlisted compose_service
TTL
operation_id
requires_confirmation
allowed_targets
```

模块不能通过向 `module.yaml` 添加 command-like 字段让自己变成可执行代码。
