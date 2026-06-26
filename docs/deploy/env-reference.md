# 环境变量参考

> 文档状态：当前实现
> 适用范围：部署 / 开发 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档汇总 OJOS 关键环境变量，说明哪些变量属于 Control Plane、Frontend 和 Worker Node。

## 2. 适用范围

适用于部署 `.env`、`frontend/.env`、`deploy/worker/.env` 和排查配置问题。

## 3. 当前实现

当前提供 `.env.example`、`frontend/.env.example` 和 `deploy/worker/.env.example`。生产环境必须复制并替换占位值。

## 4. 目标设计

后续可接入 secret manager，但服务仍应通过环境或配置读取，不在代码中写死。

## 5. 关键变量

Control Plane：`JWT_SECRET`、`INTERNAL_HMAC_KEY`、`POSTGRES_DSN`、`REDIS_ADDR`、`ARTIFACT_ROOT`、`OJOS_WORKER_TOKEN`。

Frontend：`VITE_API_BASE_URL`。

Worker：`OJOS_WORKER_ID`、`OJOS_WORKER_NAME`、`OJOS_CONTROL_PLANE_URL`、`OJOS_WORKER_TOKEN`、`OJOS_MAX_CONCURRENCY`、`OJOS_WORK_DIR`、`OJOS_SUPPORTED_LANGUAGES`、`OJOS_HEARTBEAT_INTERVAL`、`OJOS_TASK_LEASE_TTL`、`OJOS_ARTIFACT_CACHE_DIR`、`OJOS_LOG_LEVEL`。

## 6. 配置说明

示例文件只提供结构，不提供生产 secret。部署前必须替换所有占位值。

变量分区：

| 区域 | 文件 | 说明 |
| --- | --- | --- |
| Control Plane | `.env` | Gateway、Auth、Problem API、Judge API、PostgreSQL、Redis、artifact storage |
| Frontend | `frontend/.env` | 只包含 `VITE_API_BASE_URL` 等浏览器可见变量 |
| Worker Node | `deploy/worker/.env` | Gateway URL、worker token、并发、语言、work dir |

浏览器可见变量不能放 secret。worker node 变量不能包含 DB/Redis 凭据。Control Plane 变量中的 DSN 和 key 不应输出到健康检查、日志或文档。

## 7. 安全边界

不得提交 `.env`。健康检查不能返回环境变量明文。worker token 泄露后必须轮换。

## 8. 验收方式

执行 compose config 和静态验证，确认变量完整、无危险默认值。

最小检查：

```powershell
docker compose --env-file .env.example -f deploy/compose/docker-compose.yml config
docker compose --env-file deploy/worker/.env.example -f deploy/worker/docker-compose.yml config
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

如果配置扫描命中危险默认 secret，应替换为占位说明或删除默认值。生产运行前还要确认 `.env` 没有进入 Git 状态。

## 9. 常见问题

- 前端请求错地址：检查 `VITE_API_BASE_URL`。
- worker 401：检查 `OJOS_WORKER_TOKEN`。
- 服务连不上 DB：检查 `POSTGRES_DSN` 仅在 Control Plane 内使用。

## 10. 相关文档

- [Worker Token](../security/worker-token.md)
- [生产加固](production-hardening.md)
