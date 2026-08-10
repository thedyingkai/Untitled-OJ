# 部署清单

本清单用于 beta 或首个受控生产候选。按顺序执行；P0/P1 项失败时停止放量。

## 环境要求

- Linux 主机或带 Docker Desktop / Docker Engine 的 WSL2 主机。
- Docker Compose v2。
- 每个生产数据库使用 PostgreSQL 17 兼容服务。
- Redis 8.8 兼容服务，启用密码认证和持久化。
- MinIO `RELEASE.2025-09-07T16-13-09Z` 或兼容的 S3 端点。
- B 节点由已注册的 Orchestrator Agent 运行 Judge Worker；镜像提供 `nsjail`，节点必须支持 cgroup v2，并在本地策略中允许签名的 `judge-sandbox-v1` profile/digest。
- 运维脚本工具链：`bash`、`curl`、`jq`、`docker`、`pg_dump`、`pg_restore`、`redis-cli`、`mc`、`sha256sum`。
- 从源码构建 Web UI 时使用 Node.js 24.11；CI 和 Dockerfile 采用同一版本。
- 在企业代理后运行本地演练或健康探测时，配置 `NO_PROXY=localhost,127.0.0.1,::1`。

## 密钥配置

从 `.env.production.example` 而非 `.env.example` 创建生产 env 文件。

必需的生产密钥：

- `JWT_SECRET`：至少 32 字符。
- `AUTH_INTERNAL_TOKEN`：至少 32 字符。
- `ORCHESTRATOR_INTERNAL_TOKEN`：至少 32 字符。
- B 节点准备持久 Agent identity/ledger、控制面 CA、私有 Registry 凭据和 `--runtime-policy` 文件；Node 只通过 SPIFFE mTLS pull Agent API，不配置旧 push endpoint/token 或通用 service driver。
- `runtime-policy` 必须精确允许 `judge-sandbox-v1` profile digest 与受信任 Catalog 选中的 `repository@sha256`；浮动 tag、通配仓库、裸 digest 或安装请求自定义 HostConfig 均拒绝。
- `ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1`：生产环境强制校验 release 包。
- `ORCHESTRATOR_AUTH_WORKLOAD_TOKEN`：Auth 与控制面专用的 workload 签发凭据，不能复用 Auth admin 或 Orchestrator internal token，且不得下发到 Agent。
- `OJOS_WORKLOAD_PRIVATE_KEY_FILE`、`OJOS_WORKLOAD_PUBLIC_KEY_FILE`：同一 Ed25519 密钥对；私钥只挂载给 Auth，公钥只挂载给 Gateway。
- `ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN`、`ORCHESTRATOR_GATEWAY_WORKLOAD_CA_CERT`：B 节点可访问的 HTTPS Gateway 与其 CA bundle。
- `OJOS_WORKER_TOKEN` 和各 `OJOS_*_SERVICE_TOKEN` 只属于显式 `legacy-development` Compose profile，不是 Store production 配置。
- `AUTH_POSTGRES_PASSWORD`、`PROBLEM_POSTGRES_PASSWORD`、`JUDGE_POSTGRES_PASSWORD`、`USER_POSTGRES_PASSWORD`、`ORCHESTRATOR_POSTGRES_PASSWORD`：至少 20 字符。
- `AUTH_DATABASE_URL`、`PROBLEM_DATABASE_URL`、`JUDGE_DATABASE_URL`、`USER_DATABASE_URL`、`ORCHESTRATOR_DATABASE_URL`：密码认证的 PostgreSQL URL，不使用默认 `postgres` 用户。预检无法识别其它被授予 `rolsuper` 的角色，上线前还要查询 `pg_roles` 核对。
- `REDIS_PASSWORD` 和 `REDIS_URL`：密码认证的 Redis URL。
- `MINIO_ROOT_USER`、`MINIO_ROOT_PASSWORD`、`MINIO_ACCESS_KEY`、`MINIO_SECRET_KEY`。
- 标准 preflight 会检查仓库内默认监控 Compose，因此要求 `OJOS_ALERT_WEBHOOK_URL` 和 `GRAFANA_ADMIN_PASSWORD`。明确不部署监控时，设置 `OJOS_SKIP_MONITORING_CHECKS=1`；否则自定义监控 Compose 路径缺失会直接失败，避免路径拼错后悄悄跳过检查。
- 可选的传输安全强制：设置 `OJOS_SECRET_CHECK_REQUIRE_TLS=1` 时，`REDIS_URL` 必须为 `rediss://`，`MINIO_USE_SSL` 必须为 `true`（默认关闭，取决于 PKI/证书决策）。

不要把 `AUTH_INTERNAL_TOKEN` 当作 workload 或业务服务凭据。Service Contract v2 的调用权限来自已应用
ApiBinding，Agent 用 Node mTLS 兑换每 Deployment 独立的 15 分钟 JWT；容器只读取只读 service context 与
轮换 token 文件。生产 B 节点不接收 Auth/Gateway admin token、共享 worker token 或 A 机中间件凭据。

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
  orchestrator auth-service storage-service gateway problem-service judge-api user-service
```

5. 启动数据库和基础设施。
6. 在开放流量前运行迁移。
7. 启动服务：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml up -d
```

Compose 启动 A 机控制面和业务服务，但不会启动 Judge Worker。B 机安装并注册 Agent 后，通过 Store 选择 B
节点，确认 `judge_control` 与 `storage_get` Binding，再由 Agent 按固定 profile 创建并健康验证 Worker。
`judge-worker` Compose 服务只在显式 `--profile legacy-development` 时存在，禁止作为生产部署步骤。
完整注册、网络边界、Catalog 和门禁流程见 [Judge Worker 生产部署](../../deploy/worker/README.md)与
[A/B 跨机门禁](../../deploy/cross-machine/README.md)。

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
| nsjail 不可用 | Orchestrator Deployment health evidence 与 Agent 日志 | 修复 B 节点或发布新的签名 runtime profile；不要绕过 `judge-sandbox-v1` |
| Redis 不可用 | `redis-cli -u "$REDIS_URL" ping` | 检查密码、网络、持久化和 stream group |
| MinIO 不可用 | `curl /minio/health/live` 或 `mc ls` | 检查凭据、端点、bucket 和 policy |
| gateway 路由缺失 | 编排器路由表 | 重新安装 release 或重载 gateway 路由 |
| 权限拒绝 | Deployment Binding、Topology revision/generation 与 Gateway/Auth 投影 | 核对活动 Link、Release API/version、workload JWT audience/generation；不要回退 admin bearer |
| worker pending 不被消费 | Judge 队列状态、Deployment/Binding health 和 Agent Operation 日志 | 检查 Worker 注册/心跳、长轮询、`judge-sandbox-v1`、Gateway 可达性和 workload credential；B Worker 不直连 Redis，也没有生产共享 worker token |
