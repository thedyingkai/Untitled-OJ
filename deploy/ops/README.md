# 生产运维脚本

本目录放生产配置检查、备份恢复和演练脚本。脚本产出的本地 artifact 便于排错；发布证据仍要绑定到候选 commit 的 GitHub Actions run。

## 上线前检查

先从 `deploy/ops/production.env.example` 准备独立环境文件，再执行：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
deploy/ops/preflight.sh
```

`preflight.sh` 会运行密钥策略、渲染生产 Compose、检查 judge-worker 的 nsjail/cgroup 配置，并验证业务与监控 Compose。标准执行会同时要求告警 webhook 与 Grafana 管理密码。明确不部署监控时设置 `OJOS_SKIP_MONITORING_CHECKS=1`；否则监控 Compose 路径缺失会直接失败。它需要 Bash 和 Docker Compose。

`secret-check.sh` 接受直接环境变量，也接受对应的 `VAR_FILE`。它会拒绝空值、仓库内开发默认值、占位符、身份 token 复用、生产 localhost 数据库 URL 和 URL 中的默认 `postgres` 用户。它不能判断其它数据库角色是否拥有 `rolsuper`，这项权限仍需在 PostgreSQL 中核对。生产还必须启用：

```text
ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1
```

设置 `OJOS_SECRET_CHECK_REQUIRE_TLS=1` 后，Redis 必须使用 `rediss://`，MinIO 必须设置 `MINIO_USE_SSL=true`。

## 备份与恢复

备份全部状态：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
deploy/ops/backup.sh
```

恢复必须给出备份目录和确认串：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/20260702T120000Z \
OJOS_CONFIRM_RESTORE=restore-production \
deploy/ops/restore.sh
```

对指定 Operation 做真实回滚演练：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL=https://orchestrator.example.com \
OJOS_ROLLBACK_OPERATION_ID=op-release-install-20260702 \
OJOS_CONFIRM_ROLLBACK=rollback-op-release-install-20260702 \
OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1 \
deploy/ops/rollback-drill.sh
```

最后一项会授权固定的本地进程或 Compose 回滚动作。只回滚 store 记录时不要设置它。

也可以按 Service 发起 `release.rollback`：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL=https://orchestrator.example.com \
OJOS_ROLLBACK_SERVICE=judge-api \
OJOS_ROLLBACK_TARGET_OPERATION_ID=op-judge-api-install-20260702 \
OJOS_CONFIRM_ROLLBACK=rollback-judge-api \
OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1 \
deploy/ops/rollback-drill.sh
```

`OJOS_ROLLBACK_OPERATION_ID` 和 `OJOS_ROLLBACK_SERVICE` 只能选一个。Release 模式必须授权 driver；
`OJOS_ROLLBACK_TARGET_OPERATION_ID` 可精确指定一次成功的 `release.install`。不指定它时，脚本默认回滚该 Service
最近一次成功安装；也可用 `OJOS_ROLLBACK_RELEASE_VERSION` 把候选范围限定到某个版本。

## 演练脚本

- `staging-drill.sh`：临时 PostgreSQL 与 MinIO 的备份、恢复和 release rollback。
- `service-credential-drill.sh`：真实 auth migration 上的 allow、deny、revoke、expire 矩阵。
- `redis-recovery-drill.sh`：Redis Stream pending claim/recovery 和 AOF 重启。
- `alert-firing-drill.sh`：Prometheus 规则触发与 Alertmanager webhook。
- `manager-smoke.sh`：构建 Web UI，启动真实 daemon，检查静态入口和核心 API，再跑 TUI 最小测试。
- `trace-e2e-drill.sh`：提交判题任务，查询 Jaeger，并记录 Redis worker 边界的 trace 元数据。
- `basic-load-soak.sh`：覆盖登录、题目、对象存储、判题和结果查询的短时冒烟，记录成功率、延迟、队列 pending 和 worker processed。

`basic-load-soak.sh` 支持 `OJOS_LOAD_MAX_P95_MS`，但它不是容量或 SLA 证明。

## 监控

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
docker compose \
  --env-file /etc/ojos/production.env \
  -f deploy/ops/monitoring/docker-compose.yml \
  up -d
```

当前远端门禁状态见 [生产就绪证据](../../docs/production-readiness.md)。不要根据脚本存在与否判断某项演练已经通过。
