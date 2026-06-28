# API 文档总览

> 文档状态：当前实现
> 适用范围：前端对接 / 后端开发 / E2E 验收 / 安全审计
> 最后更新：2026-06-27

## 1. 入口原则

浏览器、worker 和验收脚本访问 OJOS API 时均应通过 Gateway 的 `/api` 前缀。内部服务端口不是公开 API，前端也不得写死内部服务地址。

当前主要 API 分组：

| 分组 | 示例 | 认证方式 | 浏览器可用 |
| --- | --- | --- | --- |
| Auth API | `/api/auth/login`, `/api/auth/profile` | JWT | 是 |
| Problem API | `/api/problem/problems` | JWT / 公开读策略 | 是 |
| Judge API | `/api/judge/submissions` | JWT | 是 |
| Admin API | `/api/admin/health`, `/api/judge/admin/tasks` | JWT + 后端权限 | 仅管理员 |
| Worker API | `/api/judge/worker/tasks/claim` | `X-OJOS-Worker-Token` | 否 |

## 2. 前端接入

前端统一通过 `frontend/src/api/client.ts` 调用 API。UI 页面必须使用真实 API 响应，不允许 fake/mock/random 数据。

前端应处理：

- 401：清理登录态并跳转 `/login`。
- 403：显示权限不足或跳转 `/403`。
- 404：显示资源不存在。
- 409：显示状态冲突或 lease 冲突。
- 5xx：显示可重试错误提示。

## 3. 安全边界

- User/Admin API 使用 JWT。
- Worker API 使用 worker token 和 task lease。
- 后端权限校验是安全边界，前端隐藏按钮只是体验优化。
- API 响应不得泄露 `code_path`、`result_path`、`package_dir`、`stdout_path`、`stderr_path`、`checker_log_path` 或宿主机绝对路径。

## 4. 文档索引

- [Auth API](auth-api.md)
- [Problem API](problem-api.md)
- [Judge API](judge-api.md)
- [Worker API](worker-api.md)
- [Admin API](admin-api.md)

## 5. 验收方式

静态 build 不能替代运行时 API 验收。Docker Control Plane 可用时执行：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build
powershell -NoProfile -File scripts\e2e-api.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123 `
  -WorkerToken $env:OJOS_WORKER_TOKEN
```

该脚本覆盖 auth、problem、judge submissions、admin health、admin judge、module registry、worker register/heartbeat/claim、权限拒绝和内部路径泄露扫描。
