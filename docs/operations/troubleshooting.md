# 故障排查

> 文档状态：当前实现
> 适用范围：运维 / 开发 / 验收
> 最后更新：2026-06-26

## 2026-06-27 Admin Health degraded 排查补充

`/api/admin/health` 会聚合 `gateway`、`auth`、`problem-api`、`judge-api`、`postgres`、`redis`、`storage`、`worker` 和 `queue` 等子项。正常 Docker Control Plane 中，`judge-api` 必须提供无前缀 `GET /health`，Gateway 的 Admin Health 通过内部服务地址探测它。

如果整体状态为 `degraded` 且 `judge` 子项消息为 `404 Not Found`，先在容器内验证：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml exec judge-api wget -qO- http://localhost:8082/health
docker compose --env-file .env -f deploy\compose\docker-compose.yml exec gateway wget -qO- http://judge-api:8082/health
```

两条命令都应返回 `{"status":"ok"}`。如果返回 404，说明 `judge-api` 的 `/health` 未注册或注册在错误路由组；如果直连正常但 Admin Health 异常，再检查 Gateway proxy route 的 `Target`、compose 服务名和端口。`degraded` 应代表真实子项异常，不应由错误探测路径造成。

## 1. 文档目的

本文档提供 OJOS 常见故障的排查入口。它不替代日志系统和监控系统，但能帮助维护者按服务边界快速定位问题。

## 2. 适用范围

适用于本地开发、静态验证、Control Plane 部署、worker 部署和 E2E 验收失败后的初步排查。

## 3. 当前实现

当前可排查的核心链路包括 Gateway、Auth、Problem API、Judge API、PostgreSQL、Redis、artifact storage、Worker Link 和前端页面。

## 4. 目标设计

后续应结合结构化日志、request_id、submission_id、worker_id 和审计日志，形成更完整的运维 playbook。

## 5. 关键流程

先确认失败发生在哪一层：前端构建、Gateway 代理、内部服务、数据库、Redis、worker、sandbox 或权限系统。不要直接修改最终状态，应先保留失败命令和日志。

## 6. 配置说明

常见配置包括 `.env`、`services/*/etc/*.yaml`、`deploy/compose/docker-compose.yml`、`deploy/worker/docker-compose.yml` 和 worker `.env`。

## 7. 安全边界

排查日志时不能把 secret、token、用户源码和私有题目包写入正式文档或提交到 Git。临时日志放入 `.tmp/agent/logs/`。

## 8. 验收方式

排查后重新执行失败命令。例如：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

或在对应服务目录执行更小范围命令。

## 9. 常见问题

- 静态验证失败：进入对应服务目录单独执行失败命令。
- Docker build 失败：执行 `docker version`，确认 daemon 可用。
- worker offline：检查 token、`OJOS_CONTROL_PLANE_URL`、heartbeat 和 worker 日志。
- submission 长时间 `JUDGING`：检查 `lease_expires_at`、task heartbeat、admin queue 和 worker 状态。
- 普通用户能访问 admin：检查后端权限中间件。

## 10. 相关文档

- [工程验收总入口](../e2e/e2e-engineering-acceptance.md)
- [Judge Worker 集群](../judge/judge-worker-cluster.md)
- [健康检查](health-checks.md)
## 2026-06-26 Docker API 验收排查补充

如果 `scripts\e2e-api.ps1` 失败，先查看 `.tmp/agent/reports/api-runtime/failures.txt`、`runtime-results.json` 和 `.tmp/agent/logs/api-runtime/compose-logs.txt`。常见分界：

- 401/403 状态异常：检查 Gateway JWT、Auth 角色、后端权限中间件和 HTTP status 是否真实返回。
- Worker claim 失败：先停掉 compose 自带 worker 后创建新的 pending submission，避免任务被真实 worker 抢走。
- Artifact 下载失败：检查 claim 返回的 artifact `url`、`sha256`、`size_bytes` 和 task lease。
- 前端 Network Error：检查 Gateway CORS/OPTIONS，尤其是 `/api/admin/*` 这类 Gateway 自有路由。
- 内部暴露失败：以 `docker compose ps --format json` 的 published port 为准，不要把宿主机同端口的其他进程误判为 compose 暴露。

静态验证失败请回到 `scripts\verify-static.ps1 -SkipDockerBuild`；Linux nsjail/cgroup 或多机 worker 问题请回到 `scripts/e2e-linux.sh` 和 worker 节点日志。
