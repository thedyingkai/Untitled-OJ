# Admin API

> 文档状态：当前实现
> 适用范围：管理后�?/ 运维 / 安全
> 最后更新：2026-06-26

## 2026-06-27 Admin Health judge-api 修复说明

`GET /api/admin/health` �?Gateway 聚合健康检查入口，管理�?token 访问返回 200，普通用户返�?403，无 token 返回 401。响应会检�?`gateway`、`auth`、`problem`、`judge`、`postgres`、`redis`、`artifact storage`、`internal auth key`、`workers` �?`queue` 等子项�?
其中 `judge` 子项通过 compose 内部地址探测 `judge-api` 的真�?`GET /health`，当前目标为 `http://judge-api:8082/health`。该检查不应走 public `/api/judge/*` 路由，也不应被业务路由、用户鉴权、worker token �?internal HMAC middleware 吞掉。`judge-api` 正常时不应返�?404；若 Admin Health �?judge 404 变为 `degraded`，应优先检�?`judge-api` 是否注册了无前缀 `/health`�?
`degraded` 表示真实子项异常，不应由错误探测路径造成。health 响应不得泄露 DSN、secret、worker token、HMAC key 或内部绝对路径�?
## 1. 文档目的

本文档说明管理员 API 的范围、权限要求和安全边界。管理员 API 影响系统健康、评测队列、worker、用户角色和资源授权�?
## 2. 适用范围

适用于维�?`/admin/*` 前端页面、`Gateway` admin health、`Auth` 权限管理�?`Judge API` admin judge 的开发者�?
## 3. 当前实现

当前包含健康检查、队列状态、worker 列表、task 列表、drain、requeue、用户列表、角色列表、权限点列表、permission check、审计查询，以及 Module Registry v0 只读查询�?
主要端点�?
| 方法 | 路径 | 服务 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/health` | gateway | 聚合服务健康 |
| `GET` | `/api/admin/modules` | gateway | 模块列表 |
| `GET` | `/api/admin/modules/sets` | gateway | 模块集合列表 |
| `GET` | `/api/admin/modules/topology` | gateway | 模块拓扑 |
| `GET` | `/api/admin/modules/runtime-snapshot` | gateway | Kernel Module Runtime snapshot |
| `GET` | `/api/admin/modules/:id` | gateway | 模块详情 |
| `GET` | `/api/judge/admin/queue` | judge-api | 队列�?Redis signal 指标 |
| `GET` | `/api/judge/admin/workers` | judge-api | worker 列表 |
| `POST` | `/api/judge/admin/workers/:id/drain` | judge-api | worker 下线 |
| `POST` | `/api/judge/admin/submissions/:id/requeue` | judge-api | 重新入队 |
| `GET` | `/api/auth/admin/users` | auth | 用户列表 |
| `POST` | `/api/auth/admin/permission-check` | auth | 权限检�?|
| `GET` | `/api/auth/admin/audit-logs` | auth | 审计日志 |

## 4. 目标设计

后续可扩�?installer v0、批量授权和更完整的审计导出，但后端权限校验不能弱化。当前模�?API 只读，不提供安装、启用、禁用、升级、卸载�?
## 5. 关键流程

管理员请求经 Gateway 校验 JWT 和权限。对�?judge 管理操作，Judge API 写入任务�?worker 状态；对于权限操作，Auth API 写入角色绑定和审计日志�?
## 6. 配置说明

admin API 依赖 JWT、权限数据库、内�?HMAC 和各服务健康配置。健康检查不能返�?secret�?
健康检查可以返回服务名、状态、延迟、错误摘要和配置是否存在，但不能返回 DSN、HMAC key、JWT secret、worker token 或数据库密码。队列页面可以展�?stream length、pending count、trim 策略、worker online count �?stale task 数量�?
## 7. 安全边界

普通用户访问必须返�?403。前端隐藏按钮不是安全边界。requeue、drain 和授权变更应记录审计�?
## 8. API 示例

```http
GET /api/admin/health
GET /api/admin/modules
GET /api/admin/modules/sets
GET /api/admin/modules/topology
GET /api/admin/modules/runtime-snapshot
GET /api/admin/modules/ojos.judge-core
GET /api/judge/admin/workers
POST /api/judge/admin/workers/:id/drain
GET /api/auth/admin/users
POST /api/auth/admin/permission-check
```

Module Registry v0 �?topology 响应不应为空。完�?`000009_module_registry` 迁移并启�?Gateway 后，`/api/admin/modules/topology` 至少应返回以下摘要：

```json
{
  "sets": [
    { "set_id": "kernel", "name": "Kernel Set" },
    { "set_id": "platform", "name": "Platform Set" },
    { "set_id": "core-capability", "name": "Core Capability Set" }
  ],
  "nodes": [
    { "module_id": "ojos.kernel.installer", "status": "ENABLED" },
    { "module_id": "ojos.kernel.module-runtime", "status": "ENABLED" },
    { "module_id": "ojos.kernel.module-registry", "status": "ENABLED" },
    { "module_id": "ojos.kernel.topology", "status": "ENABLED" },
    { "module_id": "ojos.platform.gateway", "status": "ENABLED" },
    { "module_id": "ojos.platform.web-shell", "status": "ENABLED" },
    { "module_id": "ojos.platform.identity-access", "status": "ENABLED" },
    { "module_id": "ojos.judge-core", "set_id": "core-capability", "status": "ENABLED" }
  ],
  "edges": [
    { "from_module_id": "ojos.judge-core", "to_module_id": "ojos.platform.web-shell", "edge_type": "requires" },
    { "from_module_id": "ojos.judge-core", "to_module_id": "ojos.platform.identity-access", "edge_type": "requires" },
    { "from_module_id": "ojos.judge-core", "to_module_id": "ojos.kernel.module-runtime", "edge_type": "requires" }
  ],
  "components": [
    { "module_id": "ojos.kernel.installer", "component_id": "installer-service", "component_type": "backend_service" },
    { "module_id": "ojos.kernel.module-runtime", "component_id": "runtime-snapshot", "component_type": "kernel_api" },
    { "module_id": "ojos.judge-core", "component_id": "problem-api", "component_type": "backend_service" },
    { "module_id": "ojos.judge-core", "component_id": "judge-api", "component_type": "backend_service" },
    { "module_id": "ojos.judge-core", "component_id": "judge-worker", "component_type": "worker_service" }
  ]
}
```

如果该接口返回空数组，应优先检�?Gateway 启动时的 `moduleregistry.BootstrapBuiltin`、PostgreSQL 连接、`module_*` 表数据，以及 `/api/admin/modules/topology` 是否�?`/api/admin/modules/:id` 之前注册�?
错误语义�?
| 状态码 | 场景 | 处理方式 |
| --- | --- | --- |
| 401 | 未登�?| 跳转登录 |
| 403 | 非管理员或缺少权�?| 显示 403 |
| 404 | worker/submission/user 不存�?| 显示资源不存�?|
| 409 | requeue/drain 状态冲�?| 显示冲突原因 |
| 500 | 下游服务异常 | health 页面显示异常服务 |

## 9. 常见问题

- health 无数据：检�?Gateway 聚合逻辑�?- worker drain 不生效：检�?worker heartbeat �?drain 状态�?- 授权后不生效：刷�?`/api/auth/me` 并检查后端权限�?
## 10. 相关文档

- [健康检查](../operations/health-checks.md)
- [权限管理安全](../security/permission-admin.md)
## 2026-06-26 Admin API 运行时验收补�?
Admin API 必须通过 Gateway 验证 admin 成功、普通用�?403、无 token 401。`scripts\e2e-api.ps1` covers `/api/admin/health`, `/api/admin/modules`, `/api/admin/modules/sets`, `/api/admin/modules/topology`, `/api/admin/modules/runtime-snapshot`, and `/api/admin/modules/:id`、`/api/judge/admin/*` �?`/api/auth/admin/*` 的核心路径�?
Module Registry topology 验收必须返回非空真实数据，至少包�?`ojos.judge-core`、kernel 内置模块、judge-core �?kernel 的依赖，以及 problem-api、judge-api、judge-worker、frontend routes、gateway routes、permissions 等组件。不得再把空数组响应写成通过�?# 2026-06-27 Module Installer Admin API

Module Installer 对外只通过 Gateway 暴露，所有端点都要求管理员角色或 `system.admin` 权限。普通用户必须返�?403，无 token 必须返回 401。Gateway 会向内部 Rust `module-installer` service 透传 actor 信息，但不会泄露 internal service URL �?internal token�?
新增端点�?
| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/modules/discover` | 发现本地 `modules/*/module.yaml` |
| `POST` | `/api/admin/modules/validate` | 校验本地 manifest �?manifest JSON |
| `POST` | `/api/admin/modules/plan` | 生成 install dry-run plan |
| `POST` | `/api/admin/modules/install` | dry-run �?apply metadata install |
| `POST` | `/api/admin/modules/:id/enable` | 启用模块 |
| `POST` | `/api/admin/modules/:id/disable` | 禁用模块，kernel/judge-core 受保�?|
| `POST` | `/api/admin/modules/:id/upgrade-plan` | 生成 upgrade plan |
| `POST` | `/api/admin/modules/:id/rollback-plan` | 生成 rollback plan |
| `POST` | `/api/admin/modules/:id/uninstall-dry-run` | 生成 uninstall dry-run plan |
| `GET` | `/api/admin/modules/:id/health` | 查询 installer 视角的模块健�?|
| `GET` | `/api/admin/modules/:id/operations` | 查询 module operation history |

请求示例�?
```json
{
  "manifest_path": "modules/demo-module/module.yaml",
  "dry_run": true
}
```

plan 响应包含 `actions`、`affected_tables`、`affected_modules`、`dependencies`、`blocked_by`、`warnings`、`dry_run` �?`can_apply`。如�?`blocked_by` 非空，apply 操作会被拒绝。v0 不支持远程市场、不执行 hook、不加载动�?bundle。`.ojosmod` 包只�?checksum integrity，signature / trust policy 留到 v1�?
Installer internal API 错误结构稳定为：

```json
{
  "error": {
    "code": "OPERATION_LOCK_HELD",
    "message": "module operation lock is held",
    "severity": "error",
    "details": {}
  }
}
```

Gateway 对外错误映射必须保持�?
| 状态码 | 场景 |
| --- | --- |
| 400 | manifest/path/validation/package error |
| 401 | �?token |
| 403 | 普通用户或权限不足 |
| 404 | 模块不存�?|
| 409 | operation lock �?dependency conflict |
| 503 | installer internal service 不可�?|

错误响应不得泄露 Rust panic、SQL 错误、internal installer URL、DB 连接串、token 或本机绝对路径�?
## Kernel Runtime Wiring v1 Admin APIs

Runtime Snapshot and route table endpoints are admin-only. Ordinary users must receive 403, and requests without a token must receive 401.

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/api/admin/modules/runtime-snapshot` | Active Runtime Snapshot for ENABLED modules. |
| `GET` | `/api/admin/modules/runtime-snapshot?include_disabled=true` | Admin inspection view including disabled registry contributions. |
| `GET` | `/api/admin/modules/runtime/routes` | Registry-driven gateway route table with conflicts and warnings. |
| `POST` | `/api/admin/modules/runtime/reload` | Rebuild and validate the runtime route table. V1 does not remove compatibility proxy routes. |

`/api/admin/modules/topology` is generated from Runtime Snapshot and returns runtime `nodes`/`edges` plus compatibility `module_nodes`/`dependency_edges` fields.

The route table may show `target_service` to admins. It must not be available to ordinary users, and the Gateway must not expose internal service URLs or tokens in errors.

## Hotplug L1 Dynamic Gateway Proxy APIs

Runtime route table endpoints are admin-only. Ordinary users receive 403, and requests without a token receive 401.

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/api/admin/modules/runtime/routes` | Registry-driven route table for enabled module gateway routes. |
| `GET` | `/api/admin/modules/runtime/routes?include_disabled=true` | Admin inspection view that includes disabled route metadata. |
| `POST` | `/api/admin/modules/runtime/reload` | Rebuild, validate and atomically replace the live runtime route table. |

Route table items include `route_id`, `module_id`, `prefix`, `service_id`, `auth_mode`, `methods`, `enabled`, `proxy_enabled`, `priority`, `status`, `conflicts`, `warnings` and `blocked_by`. `upstream_base` is hidden by default. It is only for tightly controlled debug/admin-internal inspection and must not be visible to ordinary users.

Module manifests may reference only `service_id`; they must not declare arbitrary `target_url` values. Gateway resolves `service_id` through its trusted service map. This prevents manifest-driven SSRF to public hosts, localhost, Docker socket or undeclared internal ports.

Reserved prefixes cannot be claimed by module routes: `/api/auth`, `/api/admin/modules`, `/api/admin/health`, `/api/health`, `/api/internal`, `/api/judge/worker`.

Auth modes are explicit: `public` requires no JWT; `user` requires a valid JWT; `admin` requires an admin role or `system.admin`; `worker` and `internal` are not public dynamic proxy surfaces.
## Hotplug L2 Runtime Services APIs

Runtime service APIs are admin-only. Ordinary users receive 403 and missing tokens receive 401.

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/api/admin/runtime/services` | Lists runtime services and workers from enabled module declarations. |
| `GET` | `/api/admin/runtime/services/:id` | Returns one service/worker state, health, lifecycle and route metadata. |
| `POST` | `/api/admin/runtime/services/:id/plan-start` | Generates a start plan. |
| `POST` | `/api/admin/runtime/services/:id/plan-stop` | Generates a stop plan. |
| `POST` | `/api/admin/runtime/services/:id/plan-restart` | Generates a restart plan. |
| `POST` | `/api/admin/runtime/services/:id/plan-reload` | Generates a reload plan. |
| `POST` | `/api/admin/runtime/reload` | Rebuilds runtime route table and returns route status. |

Service items include `service_id`, `module_id`, `kind`, `lifecycle`, `runtime`, `state`, `health`, `required`, `routes`, `health_check_id`, `blocked_by` and `warnings`. Admin responses do not expose Docker socket paths, host absolute paths, raw compose env, DSNs, tokens or internal secrets.

Plans are structured and plan-only:

```json
{
  "plan": {
    "action": "restart",
    "service_id": "problem-api",
    "driver": "compose",
    "can_apply": false,
    "commands": [
      { "tool": "compose", "args": ["restart", "problem-api"] }
    ],
    "blocked_by": []
  }
}
```

Gateway does not apply these plans in L2 foundation.
