# 运维手册

本手册用于生产候选 / beta 运维。优先做只读检查。破坏性操作仅在明确确认下使用。

## 健康

检查 compose 状态：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml ps
```

检查服务健康：

```bash
curl -fsS http://127.0.0.1:8090/health
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8081/health
curl -fsS http://127.0.0.1:8082/health
curl -fsS http://127.0.0.1:8085/health
```

如果本地健康检查返回代理错误，设置：

```bash
export NO_PROXY="${NO_PROXY:-localhost,127.0.0.1,::1},localhost,127.0.0.1,::1"
export no_proxy="$NO_PROXY"
```

## Gateway 路由存在性

查询编排器路由：

```bash
curl -fsS "$ORCHESTRATOR_URL/nodes/child-node/routes?include_upstream=true" | jq .
```

确认预期的 `api_id`、目标服务和所需权限都存在。如果路由缺失，检查服务的 `release.yaml`、重新安装 release，
并查看 operation 日志。

## Auth 权限注册

根据生产访问策略，使用 auth 数据库或 auth admin API：

```bash
psql "$AUTH_DATABASE_URL" -c "select code from permissions order by code;"
psql "$AUTH_DATABASE_URL" -c "select role_id, permission_code from role_permissions order by permission_code;"
```

如果某个服务权限缺失，确认 release install 注册了权限，并检查 auth-service 迁移状态。

## Redis 队列积压

使用 judge-api 队列状态：

```bash
curl -fsS -H 'X-Auth-Verified: true' -H 'X-Roles: admin' \
  "$JUDGE_API_URL/judge/admin/queue/status" | jq .
```

直接 Redis 检查：

```bash
redis-cli -u "$REDIS_URL" XLEN ojos:judge:task
redis-cli -u "$REDIS_URL" XPENDING ojos:judge:task ojos-judge-workers
```

## Worker 消费

检查 worker 日志：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml logs --no-color judge-worker
```

确认：

- worker 已向 judge-api 注册；
- `OJOS_RUNNER_MODE=nsjail`；
- 无反复的编译/运行时沙箱失败；
- Redis stream group 有活跃 consumer。

## MinIO 对象读写

检查 bucket 与对象访问：

```bash
mc alias set ojos "$MINIO_ENDPOINT" "$MINIO_ACCESS_KEY" "$MINIO_SECRET_KEY"
mc ls ojos/problems
printf 'probe\n' >/tmp/ojos-minio-probe.txt
mc cp /tmp/ojos-minio-probe.txt ojos/judge-artifacts/probes/ojos-minio-probe.txt
mc cat ojos/judge-artifacts/probes/ojos-minio-probe.txt
```

同时验证 storage-service：

```bash
curl -fsS http://127.0.0.1:8085/health | jq .
```

## nsjail runner

在 judge-worker 镜像内：

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml exec judge-worker nsjail --help >/tmp/nsjail-help.txt
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml exec judge-worker cat /opt/ojos/runtime-versions.txt
```

如果 nsjail 不可用，不要切换到假 runner。重建固定版本的 worker 镜像，并在 nsjail 矩阵通过前让该服务
远离生产流量。

## Pending 任务恢复

如果 pending 任务卡住：

1. 检查 `judge-api` 队列状态。
2. 检查 Redis `XPENDING`。
3. 重启 judge-worker 一次。
4. 如果任务超过 lease TTL 仍 pending，使用受支持的 worker 恢复路径认领它，或在一次性环境中运行 Redis
   恢复演练。
5. 在 submission 状态与 result stream 对账完成前，不要手动删除 stream 条目。

参考演练：

```bash
deploy/ops/redis-recovery-drill.sh
```

## 吊销 service 凭据

如可用，使用 auth 服务控制路径；否则通过受审计的维护会话更新 auth 数据库：

```sql
update service_credentials
set enabled = false, revoked_at = now(), updated_at = now()
where service_code = '<service-code>' and token_hint = '<token-hint>';
```

然后在 staging 用 service 凭据生命周期演练验证 deny 行为：

```bash
deploy/ops/service-credential-drill.sh
```

## 回滚

进行 operation 回滚：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL="$ORCHESTRATOR_URL" \
OJOS_ROLLBACK_OPERATION_ID="$OPERATION_ID" \
OJOS_CONFIRM_ROLLBACK="rollback-$OPERATION_ID" \
deploy/ops/rollback-drill.sh
```

回滚后，验证 host service 状态、endpoint 状态、API surface、有效路由、权限、凭据/授权和健康。

## 备份 / 恢复

备份：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/backup.sh
```

恢复：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/<stamp> \
OJOS_CONFIRM_RESTORE=restore-production \
deploy/ops/restore.sh
```

恢复后务必运行预检和冒烟检查。

## Trace

运行 trace 演练：

```bash
deploy/ops/trace-e2e-drill.sh
```

查询 Jaeger：

```bash
curl -fsS "$JAEGER_QUERY_URL/api/traces/$TRACE_ID" | jq .
```

预期服务包括 gateway-service、judge-api-service、storage-service 和 judge-worker。Redis Stream 传播由
trace 元数据和 judge-worker 原生 consumer span 表示。

## 告警触发

运行：

```bash
deploy/ops/alert-firing-drill.sh
```

在产物 manifest 中确认 Prometheus 规则触发和 Alertmanager webhook 投递。

## 生产密钥错误

运行：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/secret-check.sh
```

常见失败：

- 缺失密钥：设置该环境变量或 `VAR_FILE`；
- 弱占位符：轮换该值；
- localhost 数据库 URL：使用生产数据库端点；
- Redis URL 无密码：添加密码认证的 URL；
- PostgreSQL 超级用户：创建最小权限 service 用户。

修改密钥后，重启受影响的服务并重跑预检。
