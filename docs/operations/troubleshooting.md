# 故障排查

> 文档状态：当前实现
> 适用范围：运维 / 开发 / 验收
> 最后更新：2026-06-26

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
