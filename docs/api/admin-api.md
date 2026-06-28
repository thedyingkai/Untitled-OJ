# Admin API

> 文档状态：当前实现，v0.1.0 发布基线
> 适用范围：管理后台、运维、安全审计
> 最后更新：2026-06-28

Admin API 由 Gateway 统一暴露，覆盖健康检查、模块管理视图、Runtime、评测管理、用户角色和权限检查。所有管理端点必须通过 JWT 与管理员权限校验。普通用户应返回 `403`，无 token 应返回 `401`。

## 健康检查

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/health` | 聚合 Gateway、Auth、Problem API、Judge API、PostgreSQL、Redis、artifact storage、internal auth key、Worker 和 queue 状态 |

Health 响应可以包含服务名、状态、延迟、错误摘要和配置是否存在，但不能返回 DSN、HMAC key、JWT secret、worker token、数据库密码或本机绝对路径。

## Module Registry 与 Runtime

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/modules` | 模块列表 |
| `GET` | `/api/admin/modules/sets` | 模块集合 |
| `GET` | `/api/admin/modules/:id` | 模块详情 |
| `GET` | `/api/admin/modules/topology` | 从 Runtime Snapshot 派生的模块拓扑 |
| `GET` | `/api/admin/modules/runtime-snapshot` | enabled module 的 active Runtime Snapshot |
| `GET` | `/api/admin/modules/runtime-snapshot?include_disabled=true` | 管理员检查 disabled registry contribution |
| `GET` | `/api/admin/modules/runtime/routes` | 当前 runtime route table |
| `GET` | `/api/admin/modules/runtime/routes?include_disabled=true` | 包含 disabled route metadata 的管理员视图 |
| `POST` | `/api/admin/modules/runtime/reload` | 重建并校验 runtime route table |

Runtime route table 可向管理员展示 `target_service`/`service_id`，但不能向普通用户暴露内部 upstream URL 或 token。Manifest 只能声明 `service_id`，不能声明 arbitrary `target_url`。

## Module Installer 管理 API

这些 API 对外只通过 Gateway 暴露，Gateway 调用内部 Rust `module-installer` service，并注入 actor 信息。前端不直接访问 installer service。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/modules/discover` | 发现本地 `modules/*/module.yaml` |
| `POST` | `/api/admin/modules/validate` | 校验 manifest path 或 manifest JSON |
| `POST` | `/api/admin/modules/plan` | 生成 install dry-run plan |
| `POST` | `/api/admin/modules/install` | dry-run 或 metadata install apply |
| `POST` | `/api/admin/modules/:id/enable` | 启用模块 |
| `POST` | `/api/admin/modules/:id/disable` | 禁用模块，Kernel/Judge Core 受保护 |
| `POST` | `/api/admin/modules/:id/upgrade-plan` | 生成 upgrade plan |
| `POST` | `/api/admin/modules/:id/rollback-plan` | 生成 rollback plan |
| `POST` | `/api/admin/modules/:id/uninstall-dry-run` | 生成 uninstall dry-run plan |
| `GET` | `/api/admin/modules/:id/health` | 查询 installer 视角的模块健康 |
| `GET` | `/api/admin/modules/:id/operations` | 查询 module operation history |

请求示例：

```json
{
  "manifest_path": "modules/sample-hello/module.yaml",
  "dry_run": true
}
```

Plan 响应包含：

```text
actions
affected_tables
affected_modules
dependencies
blocked_by
warnings
dry_run
can_apply
```

`blocked_by` 非空时 apply 必须拒绝。v0.1.0 不支持 remote market、不执行 hook、不加载 dynamic frontend bundle。`.ojosmod` package 只保证 checksum integrity，不保证 publisher trust。

## Runtime Services

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/runtime/services` | 列出 enabled module 声明的 runtime services 和 workers |
| `GET` | `/api/admin/runtime/services/:id` | 查询一个 service/worker 的 state、health、lifecycle 和 route metadata |
| `POST` | `/api/admin/runtime/services/:id/plan-start` | 生成 start plan |
| `POST` | `/api/admin/runtime/services/:id/plan-stop` | 生成 stop plan |
| `POST` | `/api/admin/runtime/services/:id/plan-restart` | 生成 restart plan |
| `POST` | `/api/admin/runtime/services/:id/plan-reload` | 生成 reload plan |
| `POST` | `/api/admin/runtime/reload` | 重建 runtime route table 并返回 route status |
| `GET` | `/api/admin/runtime/operations` | 查询 runtime operation history |
| `GET` | `/api/admin/runtime/operations/:id` | 查询单个 runtime operation |
| `POST` | `/api/admin/runtime/plans/:id/apply` | 明确禁用，管理员收到 `501` |

Gateway/Web 只生成 plan 和查看 operations。真实 apply 使用 `ojosctl runtime apply-plan` 或未来 operator，并在本地重新校验 plan。

## Judge Admin

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/judge/admin/queue` | 队列和 Redis signal 指标 |
| `GET` | `/api/judge/admin/workers` | Worker 列表 |
| `GET` | `/api/judge/admin/tasks` | 任务列表 |
| `POST` | `/api/judge/admin/workers/:id/drain` | Worker 下线 |
| `POST` | `/api/judge/admin/submissions/:id/requeue` | 重新入队 |

这些端点必须记录审计或可追踪操作结果，不能泄露 worker token 或内部地址。

## Auth Admin

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/auth/admin/users` | 用户列表 |
| `GET` | `/api/auth/admin/roles` | 角色列表 |
| `GET` | `/api/auth/admin/permissions` | 权限点列表 |
| `POST` | `/api/auth/admin/permission-check` | 权限检查 |
| `GET` | `/api/auth/admin/audit-logs` | 审计日志 |

前端隐藏按钮不是安全边界，所有权限必须由后端校验。

## 错误语义

| 状态码 | 场景 |
| --- | --- |
| `400` | manifest/path/validation/package error |
| `401` | 无 token 或 token 无效 |
| `403` | 普通用户或权限不足 |
| `404` | 模块、Worker、Submission 或用户不存在 |
| `409` | operation lock、dependency conflict 或状态冲突 |
| `500` | 下游服务异常 |
| `503` | installer internal service 不可达 |

错误响应不得泄露 Rust panic、SQL 错误、internal installer URL、DB 连接串、token、secret、password 或本机绝对路径。

## 验收

`scripts/e2e-api.ps1` 覆盖 Admin Health、Module Registry、Module Installer、Runtime Services、Judge Admin、Auth Admin 和权限拒绝路径。必须保持：

```text
failed=0
path_leaks=0
admin_health_status=ok
admin_health_judge_status=ok
ordinary user 403
no token 401
```
