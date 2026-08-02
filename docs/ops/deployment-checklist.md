# 部署清单

本清单用于 beta 或首个受控生产候选。按顺序执行；P0/P1 项失败时停止放量。

## 环境要求

- Linux 主机或带 Docker Desktop / Docker Engine 的 WSL2 主机。
- Docker Compose v2。
- 每个生产数据库使用 PostgreSQL 17 兼容服务。
- Redis 8.8 兼容服务，启用密码认证和持久化。
- MinIO `RELEASE.2025-09-07T16-13-09Z` 或兼容的 S3 端点。
- judge-worker 镜像中提供 `nsjail`；主机必须支持所配置的 cgroup/seccomp/mount 策略。
- 运维脚本工具链：`bash`、`curl`、`jq`、`docker`、`pg_dump`、`pg_restore`、`redis-cli`、`mc`、`sha256sum`。
- 从源码构建 Web UI 时使用 Node.js 24.11；CI 和 Dockerfile 采用同一版本。
- 在企业代理后运行本地演练或健康探测时，配置 `NO_PROXY=localhost,127.0.0.1,::1`。

## 密钥配置

从 `.env.production.example` 而非 `.env.example` 创建生产 env 文件。

必需的生产密钥：

- `JWT_SECRET`：至少 32 字符。
- `AUTH_INTERNAL_TOKEN`：至少 32 字符。
- `ORCHESTRATOR_INTERNAL_TOKEN`：至少 32 字符。
- 启用 `ORCHESTRATOR_NODE_DISPATCH` 时：设置 `ORCHESTRATOR_NODE_ENDPOINT` 和独立的 `ORCHESTRATOR_NODE_TOKEN`；缺少派发地址，生产预检会失败。
- 启用 `ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER` 时：配置本机 `ORCHESTRATOR_NODE_HOST_IP`，请求目标必须与它一致。请求已授权 driver、但目标 Node 没打开该上限时，安装会失败；只有未授权 driver 的请求可以只登记元数据。
- `ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1`：生产环境强制校验 release 包。
- `OJOS_WORKER_TOKEN`：至少 32 字符。
- `OJOS_USER_SERVICE_TOKEN`、`OJOS_PROBLEM_SERVICE_TOKEN`、`OJOS_JUDGE_API_SERVICE_TOKEN`、`OJOS_JUDGE_WORKER_SERVICE_TOKEN`：每个调用方独立签发，至少 32 字符；不能彼此复用，也不能复用 JWT、内部或 worker token。
- `AUTH_POSTGRES_PASSWORD`、`PROBLEM_POSTGRES_PASSWORD`、`JUDGE_POSTGRES_PASSWORD`、`USER_POSTGRES_PASSWORD`、`ORCHESTRATOR_POSTGRES_PASSWORD`：至少 20 字符。
- `AUTH_DATABASE_URL`、`PROBLEM_DATABASE_URL`、`JUDGE_DATABASE_URL`、`USER_DATABASE_URL`、`ORCHESTRATOR_DATABASE_URL`：密码认证的 PostgreSQL URL，不使用默认 `postgres` 用户。预检无法识别其它被授予 `rolsuper` 的角色，上线前还要查询 `pg_roles` 核对。
- `REDIS_PASSWORD` 和 `REDIS_URL`：密码认证的 Redis URL。
- `MINIO_ROOT_USER`、`MINIO_ROOT_PASSWORD`、`MINIO_ACCESS_KEY`、`MINIO_SECRET_KEY`。
- 标准 preflight 会检查仓库内默认监控 Compose，因此要求 `OJOS_ALERT_WEBHOOK_URL` 和 `GRAFANA_ADMIN_PASSWORD`。明确不部署监控时，设置 `OJOS_SKIP_MONITORING_CHECKS=1`；否则自定义监控 Compose 路径缺失会直接失败，避免路径拼错后悄悄跳过检查。
- 可选的传输安全强制：设置 `OJOS_SECRET_CHECK_REQUIRE_TLS=1` 时，`REDIS_URL` 必须为 `rediss://`，`MINIO_USE_SSL` 必须为 `true`（默认关闭，取决于 PKI/证书决策）。

不要把 `AUTH_INTERNAL_TOKEN` 当作业务服务凭据。经 Gateway 的用户权限校验使用
`OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT` 和前三个 `OJOS_*_SERVICE_TOKEN`；judge-worker 的独立 token
用于访问 storage API。Compose 会显式把这些变量注入对应容器，不会从根 `.env` 自动继承未声明变量。迁移
`000012_grant_service_permission_check` 只登记 identity 和 grant，不签发 token；上线前应通过
`POST /auth/admin/services/{service_code}/credentials` 分别签发并写入密钥管理系统。

启用 `ORCHESTRATOR_GATEWAY_ROUTE_PUBLISH=1` 时，还要设置 `GATEWAY_ENDPOINT`、`GATEWAY_ADMIN_TOKEN` 和
`GATEWAY_NODE_ID`。缺少 Node ID 时，空路由表强制刷新会被拒绝。

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

Compose 会直接启动业务服务。最小 orchestrator 镜像不含业务源码、Compose 文件或 Docker CLI，不能在容器内
代替 Compose 启动其他服务。若要使用编排器的 LocalProcess/DockerCompose 生命周期驱动，应从完整源码工作区运行，
或显式挂载审核过的运行资产和 Docker 访问能力。

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

再检查 `http://127.0.0.1:8090/` 能返回 Web UI，通过 Gateway 跑一次登录、服务权限和判题冒烟。控制面 API
请求需携带 `x-ojos-orchestrator-token`。

## 回滚步骤

进行 operation 回滚：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL=https://orchestrator.example.com \
OJOS_ROLLBACK_OPERATION_ID=op-release-install-YYYYMMDD \
OJOS_CONFIRM_ROLLBACK=rollback-op-release-install-YYYYMMDD \
OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1 \
deploy/ops/rollback-drill.sh
```

`OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1` 会让脚本再次传入 `execute_service_driver=true`，授权固定的
本地进程或 Compose 回滚动作。只回滚 store 记录时不要设置它。schema、Redis、对象存储和 auth-service
外部副作用没有通用自动回滚；这类恢复应停止放量，并按备份或服务专用补偿步骤处理。

若要按 Service 发起 `release.rollback`，改用 `OJOS_ROLLBACK_SERVICE`，并保留 driver 授权。可选的
`OJOS_ROLLBACK_TARGET_OPERATION_ID` 用来锁定原安装 Operation；未指定时可用
`OJOS_ROLLBACK_RELEASE_VERSION` 限定版本。它们的完整示例见 [运维脚本说明](../../deploy/ops/README.md)。
`OJOS_ROLLBACK_OPERATION_ID` 与 `OJOS_ROLLBACK_SERVICE` 不得同时设置。

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
