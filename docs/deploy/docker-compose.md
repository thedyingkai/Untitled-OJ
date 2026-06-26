# Docker Compose 说明

> 文档状态：当前实现
> 适用范围：部署 / 开发 / 静态验证
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS compose 文件的职责、验证方式和安全边界，避免把 worker、数据库或内部服务错误暴露。

## 2. 适用范围

适用于本地 Control Plane 部署、worker node 部署和静态验证。

## 3. 当前实现

- `deploy/compose/docker-compose.yml`：Control Plane。
- `deploy/worker/docker-compose.yml`：独立 worker node。

## 4. 目标设计

compose 文件应保持 profile 清晰，生产环境可迁移到 Kubernetes 或其他编排系统，但安全边界不变。

## 5. 关键流程

先执行 config 验证，再在 Docker daemon 可用时 build/up：

```powershell
docker compose --env-file .env.example -f deploy/compose/docker-compose.yml config
docker compose --env-file deploy/worker/.env.example -f deploy/worker/docker-compose.yml config
```

建议顺序：

1. 复制 `.env.example` 为本机 `.env`，替换所有 secret 占位值。
2. 执行 Control Plane compose config。
3. 执行 worker compose config。
4. Docker daemon 可用时执行 build。
5. 启动 Control Plane 后再启动 worker。
6. 在 `/admin/health` 和 `/admin/judge` 验证服务、队列和 worker。

不要把 worker node compose 与 Control Plane compose 合并成一个依赖共享磁盘的方案。多机部署时，worker 只通过 Gateway 访问 Worker API。

## 6. 配置说明

Control Plane 使用 `.env`，worker 使用 `deploy/worker/.env`。示例文件不能包含生产 secret。

## 7. 安全边界

PostgreSQL/Redis 不公开暴露；worker compose 不包含 DB/Redis 凭据；worker 不挂载 Control Plane storage。

## 8. 验收方式

`docker compose config` 通过是静态验收；`up -d --build` 需要 Docker daemon。

静态验证命令：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

运行验证命令需要 Docker daemon：

```powershell
docker compose --env-file .env -f deploy/compose/docker-compose.yml up -d --build
docker compose --env-file deploy/worker/.env -f deploy/worker/docker-compose.yml up -d --build
```

如果 `docker version` 无法连接 daemon，只能记录 Docker 运行验收未执行；不影响文档、代码和静态验证继续推进。

## 9. 常见问题

- 变量未定义：检查 `.env.example`。
- daemon 不可用：只能跳过 build。
- worker 无法注册：检查 token 和 Gateway URL。

## 10. 相关文档

- [Control Plane 部署](deploy-control-plane.md)
- [Worker Node 部署](deploy-worker-node.md)
