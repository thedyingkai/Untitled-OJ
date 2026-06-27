# 故障排查

> 文档状态：当前实现
> 适用范围：运行时 / 开发 / 验收
> 最后更新：2026-06-27

## 1. 排查原则

先确认失败发生在哪一层：前端、Gateway、内部 API、数据库、Redis、worker、sandbox、权限系统或文档/验收脚本。不要把静态构建成功写成运行时 API 成功。

临时日志、截图、响应和报告只能写入 `.tmp/agent/`。

## 2. 前端页面问题

常用检查：

```powershell
cd frontend
npm run build
cd ..
```

如果页面空白，先检查：

- Vite dev server 是否正常。
- Route lazy import 是否构建失败。
- 浏览器是否跳转到 `/login`。
- API client 是否返回 401/403/5xx。
- 页面是否绕过统一 API client。

UI 问题应参考 [UI 风格指南](../development/ui-style-guide.md)。状态颜色不一致时，检查是否使用了 `OjosStatusTag` 和 `frontend/src/utils/status.ts`。

## 3. Admin Health degraded

`/api/admin/health` 会检查 gateway、auth、problem-api、judge-api、postgres、redis、storage、worker、queue 等子项。

正常 Docker Control Plane 中，`judge-api` 必须提供无前缀 `GET /health`，Gateway Admin Health 通过内部服务地址探测该 endpoint，不应通过 public `/api/judge/*` 路由探测。

排查命令：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml exec judge-api wget -qO- http://localhost:8082/health
docker compose --env-file .env -f deploy\compose\docker-compose.yml exec gateway wget -qO- http://judge-api:8082/health
```

两条命令都应返回 `{"status":"ok"}`。如果直接访问正常但 Admin Health degraded，检查 Gateway health probe 的内部服务名、端口和路径。

## 4. Docker API 验收失败

查看：

- `.tmp/agent/reports/api-runtime/failures.txt`
- `.tmp/agent/reports/api-runtime/runtime-results.json`
- `.tmp/agent/logs/api-runtime/compose-logs.txt`

常见分界：

- 401/403 异常：检查 JWT、角色、权限中间件和测试用户授权。
- Worker claim 异常：检查 `OJOS_WORKER_TOKEN`、Gateway worker route 和 task lease。
- Artifact 下载异常：检查 claim 返回的 `url`、`sha256`、`size_bytes` 和 lease。
- 内部路径泄露：检查 API response sanitizer，不允许返回内部绝对路径字段。

## 5. OJ 提交长期 PENDING

先看 Admin Judge：

- queue pending/scheduled/judging
- workers 是否 ONLINE
- tasks 是否有 lease
- worker 日志是否反复 claim 同一 task

本地 Docker Desktop/WSL 中，nsjail 可能需要 `OJOS_NSJAIL_NO_PIVOTROOT=true`。Linux Runtime 验收不应依赖无内存限制的兼容降级；必须确认 `/sys/fs/cgroup/cgroup.controllers` 存在，并包含 `memory` 和 `pids`。

2026-06-27 的 WSL2 Linux 验收环境使用 `Ubuntu-24.04-OJOS`，Docker worker 容器通过 host cgroup namespace 和 `/sys/fs/cgroup` mount 执行 case 级 cgroup v2 限制。该环境已通过四语言 AC/WA/CE/RE/TLE/MLE/OLE 矩阵、fork bomb、TLE 残留进程清理、双 worker 模拟、crash recovery 和 stale lease 拒绝。

如果 MLE 被误判为 TLE 或 RE，重点检查：

- worker 是否能创建 case cgroup。
- 被 nsjail 启动的子进程是否加入了 case cgroup。
- `memory.max`、`memory.peak`、`memory.events` 是否可读写。
- worker 容器是否缺少必要 capability 或 cgroup mount。

## 6. 必跑验证

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

Docker 可用时：

```powershell
powershell -NoProfile -File scripts\e2e-api.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123 `
  -WorkerToken $env:OJOS_WORKER_TOKEN
```

Linux nsjail/cgroup、多 worker、worker crash recovery 属于 Linux Judge Runtime 验收。WSL2 Linux 本机双 worker 可以作为本地运行验收，但不能替代第二台真实机器的多机 worker 验收。

Linux 环境可用时：

```bash
bash scripts/e2e-linux.sh
```

该脚本应保持 `matrix_failed=0`、`path_leaks=0`、`permission_failures=0`，并确认 `memory_kb` 不恒为 0。

## 7. 相关文档

- [前端开发指南](../development/frontend-development.md)
- [UI 风格指南](../development/ui-style-guide.md)
- [健康检查](health-checks.md)
- [工程验收总入口](../e2e/e2e-engineering-acceptance.md)
