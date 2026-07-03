# 部署清单

本清单用于首个生产候选 / beta 部署。按顺序执行，遇到任何失败的 P0/P1 项即停止。

## 环境要求

- Linux 主机或带 Docker Desktop / Docker Engine 的 WSL2 主机。
- Docker Compose v2。
- 每个生产数据库使用 PostgreSQL 17 兼容服务。
- Redis 8.8 兼容服务，启用密码认证和持久化。
- MinIO `RELEASE.2025-09-07T16-13-09Z` 或兼容的 S3 端点。
- judge-worker 镜像中提供 `nsjail`；主机必须支持所配置的 cgroup/seccomp/mount 策略。
- 运维脚本工具链：`bash`、`curl`、`jq`、`docker`、`pg_dump`、`pg_restore`、`redis-cli`、`mc`、`sha256sum`。
- 在企业代理后运行本地演练或健康探测时，配置 `NO_PROXY=localhost,127.0.0.1,::1`。

## 密钥配置

从 `.env.production.example` 而非 `.env.example` 创建生产 env 文件。

必需的生产密钥：

- `JWT_SECRET`：至少 32 字符。
- `AUTH_INTERNAL_TOKEN`：至少 32 字符。
- `ORCHESTRATOR_INTERNAL_TOKEN`：至少 32 字符。
- `OJOS_WORKER_TOKEN`：至少 32 字符。
- `AUTH_POSTGRES_PASSWORD`、`PROBLEM_POSTGRES_PASSWORD`、`JUDGE_POSTGRES_PASSWORD`、`USER_POSTGRES_PASSWORD`、`ORCHESTRATOR_POSTGRES_PASSWORD`：至少 20 字符。
- `AUTH_DATABASE_URL`、`PROBLEM_DATABASE_URL`、`JUDGE_DATABASE_URL`、`USER_DATABASE_URL`、`ORCHESTRATOR_DATABASE_URL`：密码认证的 PostgreSQL URL，不使用 `postgres` 超级用户。
- `REDIS_PASSWORD` 和 `REDIS_URL`：密码认证的 Redis URL。
- `MINIO_ROOT_USER`、`MINIO_ROOT_PASSWORD`、`MINIO_ACCESS_KEY`、`MINIO_SECRET_KEY`。
- 监控 profile：启用监控时需要 `OJOS_ALERT_WEBHOOK_URL` 和 `GRAFANA_ADMIN_PASSWORD`。
- 可选的传输安全强制：设置 `OJOS_SECRET_CHECK_REQUIRE_TLS=1` 时，`REDIS_URL` 必须为 `rediss://`，`MINIO_USE_SSL` 必须为 `true`（默认关闭，取决于 PKI/证书决策）。

预检：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh
```

## 启动步骤

1. 安装 Docker / Docker Compose 并确认 daemon 正在运行。
2. 把生产 env 文件放到 `/etc/ojos/production.env`，权限 `0600`。
3. 运行预检：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh
```

4. 构建或拉取固定版本的镜像：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml build \
  orchestrator auth-service storage-service gateway problem-service judge-api judge-worker user-service
```

5. 启动数据库和基础设施。
6. 在开放流量前运行迁移。
7. 启动服务：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml up -d
```

## 迁移步骤

显式运行迁移服务：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm orchestrator-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm auth-service-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm problem-service-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm judge-api-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm user-service-migrations
```

没有新备份和明确回滚计划时，不要运行破坏性迁移。

## 冒烟验证

运行：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml ps
curl -fsS http://127.0.0.1:8090/health
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8081/health
curl -fsS http://127.0.0.1:8082/health
curl -fsS http://127.0.0.1:8085/health
```

然后通过已部署的 gateway 或该环境现有的 compose 冒烟命令跑一次判题冒烟。

## 回滚步骤

进行 operation 回滚：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL=https://orchestrator.example.com \
OJOS_ROLLBACK_OPERATION_ID=op-release-install-YYYYMMDD \
OJOS_CONFIRM_ROLLBACK=rollback-op-release-install-YYYYMMDD \
deploy/ops/rollback-drill.sh
```

如需 schema 回滚，请停止并使用备份/恢复。当前 release 回滚是应用层的；schema 回滚不支持。

## 备份 / 恢复

备份：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/backup.sh
```

恢复需要显式确认：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/20260702T120000Z \
OJOS_CONFIRM_RESTORE=restore-production \
deploy/ops/restore.sh
```

恢复后，在重新开放流量前运行预检和冒烟检查。

## 日志

- Compose 服务日志：`docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml logs --no-color <service>`。
- 演练产物：`artifacts/<drill-name>/`。
- 编排器 operation 日志：查询编排器 operation 详情端点或 manager smoke 响应。
- Prometheus / Alertmanager / Jaeger 日志：监控 compose 日志。

## 监控与告警

启动监控：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
docker compose --env-file /etc/ojos/production.env -f deploy/ops/monitoring/docker-compose.yml up -d
```

验证：

```bash
deploy/ops/alert-firing-drill.sh
deploy/ops/trace-e2e-drill.sh
```

## 常见排障

| 症状 | 检查 | 处理 |
| --- | --- | --- |
| Docker daemon 不可用 | `docker ps` | 启动 Docker / 服务管理器并重跑预检 |
| 代理拦截本地 curl | `env | grep -i proxy` | 设置 `NO_PROXY=localhost,127.0.0.1,::1` |
| nsjail 不可用 | `docker compose logs judge-worker` | 重建 judge-worker 镜像并确认 runtime lock |
| Redis 不可用 | `redis-cli -u "$REDIS_URL" ping` | 检查密码、网络、持久化和 stream group |
| MinIO 不可用 | `curl /minio/health/live` 或 `mc ls` | 检查凭据、端点、bucket 和 policy |
| gateway 路由缺失 | 编排器路由表 | 重新安装 release 或重载 gateway 路由 |
| 权限拒绝 | auth 权限 / service grant | 核对 release 权限与 service 凭据授权 |
| worker pending 不被消费 | judge 队列状态 API | 检查 worker 注册、Redis stream group、nsjail 失败和 worker token |
