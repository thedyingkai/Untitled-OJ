# 健康检查

> 文档状态：当前实现
> 适用范围：运维 / 管理后台 / 部署验收
> 最后更新：2026-06-26

## 2026-06-27 Admin Health judge-api 探测语义

`GET /api/admin/health` 由 Gateway 聚合 Control Plane 健康状态，当前检查项包括 `gateway`、`auth`、`problem`、`judge`、`postgres`、`redis`、`artifact storage`、`internal auth key`、`workers` 和 `queue`。

Gateway 检查 `judge` 子项时必须直连 compose 内部服务 `judge-api` 的真实 `GET /health` endpoint，即 `http://judge-api:8082/health`，不得通过 public `/api/judge/*` 转发路径探测。`judge-api` 正常运行时该 endpoint 返回 `{"status":"ok"}`，Admin Health 中 `judge` 子项应为 `ok`，整体状态不应因为错误探测路径导致 `degraded`。

`degraded` 只表示真实子项异常，例如下游服务不可达、Redis/Postgres/storage 不可用、worker/queue 查询失败或内部 HMAC key 不可验证。health 响应不得返回 DSN、secret、worker token、HMAC key 或内部绝对路径。

## 1. 文档目的

本文档说明 OJOS 健康检查的检查项、展示方式和排障用途。健康检查不是简单 `ok`，而是用于判断 Control Plane、队列、worker 和 artifact storage 是否可用。

## 2. 适用范围

适用于管理员、部署人员和后端维护者。前端入口为 `/admin/health`，后端入口为 `GET /api/admin/health`。

## 3. 当前实现

健康检查通过 Gateway 暴露，管理员访问后看到各组件状态、latency、错误信息、worker online count、queue pending count 和 internal auth key status。

## 4. 目标设计

健康检查应继续扩展为可观测入口，但不能泄露敏感配置。后续可以增加版本号、build info、迁移版本和 artifact storage backend 类型。

## 5. 关键流程

管理员打开 `/admin/health`，前端携带 JWT 调用 Gateway。Gateway 执行权限检查并聚合 Auth、Problem API、Judge API、PostgreSQL、Redis、artifact storage 和 worker/queue 指标。

健康检查建议按如下字段展示：

| 组件 | 检查内容 | 失败影响 |
| --- | --- | --- |
| gateway | 自身进程、配置、内部签名配置状态 | 所有 API 入口异常 |
| auth | 登录、用户和权限 API | 登录和 admin 权限异常 |
| problem-api | 题目列表、详情和题目包能力 | 题目浏览和提交前校验异常 |
| judge-api | 提交、队列和 worker API | 提交与评测调度异常 |
| postgres | ping、查询延迟 | 用户、题目、提交、task 均异常 |
| redis | ping、stream 指标 | task signal 和队列观测异常 |
| artifact storage | 根目录或对象存储可读写 | 题目包、源码、结果不可用 |
| workers | online count、last_seen | 提交可能卡在 PENDING |
| queue | stream length、pending、stale tasks | 调度恢复能力受影响 |

health 页面应支持手动刷新和自动刷新，但不能因为自动刷新导致后台 API 被过度调用。

## 6. 配置说明

健康检查依赖服务地址、数据库连接、Redis 地址、artifact root 和内部 HMAC 配置。配置错误应显示为异常状态和可读错误，不返回 secret。

Gateway 聚合下游健康时，应带 request id，便于把前端错误、Gateway 日志和下游服务日志串起来。错误信息应足够定位组件，例如“redis ping timeout”或“problem-api unreachable”，但不显示连接串、密码、token 或本地绝对路径。

## 7. 安全边界

该 API 仅 admin 可访问。不返回 DSN、secret、worker token、HMAC key 或内部文件路径。

## 8. 验收方式

- 管理员能访问 `/admin/health`。
- 普通用户访问返回 403。
- Redis 或服务不可用时显示异常。
- 响应不包含 secret。

静态验收：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

运行验收在 Docker daemon 可用时执行。可以停掉 Redis 或 `problem-api` 容器后刷新 `/admin/health`，预期对应组件显示异常，其他组件继续显示可用。若当前环境不能停服务，应只记录“未执行”，不要把该项写成通过。

## 9. 常见问题

- health 全部失败：检查 Gateway 配置和内部 HMAC。
- Redis 异常：检查 Redis 地址、认证和网络。
- worker online 为 0：检查 Worker Link heartbeat。
- artifact 异常：检查 storage 目录权限。

## 10. 相关文档

- [Admin API](../api/admin-api.md)
- [Control Plane 部署](../deploy/deploy-control-plane.md)
