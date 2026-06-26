# API 文档总览

> 文档状态：当前实现
> 适用范围：开发 / API 对接 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档是 OJOS API 文档入口，说明 API 分组、鉴权方式、错误处理和安全边界。

## 2. 适用范围

适用于前端接入、后端开发、E2E 验收和安全审计。

## 3. 当前实现

所有浏览器和 worker 可访问 API 都通过 Gateway 的 `/api` 前缀。内部服务端口不作为公开 API。

当前 API 分三类：

| 类型 | 示例 | 认证 | 是否公开到浏览器 |
| --- | --- | --- | --- |
| 用户 API | `/api/auth/me`、`/api/problem/problems`、`/api/judge/submissions` | JWT 或匿名 public | 是 |
| 管理 API | `/api/admin/health`、`/api/judge/admin/workers`、`/api/auth/admin/users` | JWT + 后端权限 | 仅管理员 |
| Worker API | `/api/judge/worker/register`、`/api/judge/worker/tasks/claim` | worker token + lease | 仅 worker |

所有三类 API 都通过 Gateway 进入系统。`problem-api`、`judge-api` 和 `auth` 的内部端口不作为浏览器入口，也不应写进前端配置。

## 4. 目标设计

API 文档应随 public schema 同步更新。新增接口必须说明 base path、认证方式、权限要求、请求示例、响应示例和错误情况。

## 5. API 分组

- [Auth API](auth-api.md)：登录、注册、当前用户。
- [Problem API](problem-api.md)：题目、题目包校验。
- [Judge API](judge-api.md)：提交、结果、语言。
- [Worker API](worker-api.md)：worker-only 协议。
- [Admin API](admin-api.md)：健康、队列、权限。

## 6. 配置说明

前端通过 `VITE_API_BASE_URL` 指向 Gateway。worker 通过 `OJOS_CONTROL_PLANE_URL` 指向 Gateway。

API client 必须统一处理 400、401、403、404、409、429、500 等错误。后端响应中如果包含 request id，前端应在错误提示或调试信息中保留，便于跨服务排查。

请求体和响应体默认使用 JSON。文件或 artifact 传输必须由对应 API 明确声明权限、大小限制、digest 和错误行为，不能退回到传本地路径。

## 7. 安全边界

User API 使用 JWT；Worker API 使用 `X-OJOS-Worker-Token`；Admin API 使用后端权限校验；Public API 不返回内部路径。

## 8. 验收方式

前端页面必须通过真实 API 工作；E2E 脚本必须覆盖 login、problem、submission、worker 和 admin API。

静态验收检查前端是否绕过统一 API client、public schema 是否包含内部路径、部署文件是否暴露内部服务。运行验收应至少覆盖：注册、登录、刷新 `/api/auth/me`、题目列表、题目详情、提交、提交轮询、管理员健康页、worker 注册和 claim。

## 9. 常见问题

- 401：token 缺失或过期。
- 403：权限不足。
- 404：资源不存在或不可见。
- 409：状态冲突或旧 lease。

## 10. 相关文档

- [安全边界](../security/security-boundary.md)
- [路径泄露防护](../security/path-leak-prevention.md)
## 2026-06-26 API 运行时验收补充

API 文档中的接口验收必须通过 Gateway 真实请求完成。推荐命令：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build
powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN
```

该脚本覆盖 auth、problem、judge submissions、admin health、admin judge、module registry、worker register/heartbeat/claim、权限拒绝和内部路径泄露扫描。静态验证和前端 build 不能写成 API 验收通过。
