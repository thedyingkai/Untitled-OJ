# 运维手册

本手册用于 beta 和受控生产候选。先做只读检查；删除、恢复和运行时驱动操作都需要明确确认。

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

## ApiBinding、Gateway 与 Auth 投影

从 Web/TUI 的 Deployment Binding 页面查看，或使用当前人类会话读取正式接口：

```bash
curl --fail-with-body --cacert "$ORCHESTRATOR_CA_FILE" \
  --cookie "$ORCHESTRATOR_SESSION_COOKIE" \
  "$ORCHESTRATOR_URL/api/v1/deployments/$DEPLOYMENT_ID/bindings" | jq .
```

确认 requirement、API/version、consumer/provider Deployment、Topology revision、Gateway virtual endpoint、
credential/context generation、desired/observed state、health 和 drift 全部匹配。缺失时检查签名 Release v2、
当前 applied Link、provider RuntimeInstance 与 RuntimeReport，再通过 Topology draft/diff/apply 修复；不要直接写
Gateway/Auth 数据库或伪造 `X-OJOS-Caller-*` header。

Gateway 从 Deployment JWT 推导 caller 并实时校验活动 Binding/generation。Auth/Gateway 投影只由控制面管理；
远程 Agent 与容器不持有 admin credential，缺少 workload grant 时也不能回退 admin bearer。

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

在 Orchestrator 的 Deployment/Operation 日志中选择 B 节点上的 Judge Worker，不要从 A 机 Compose 查询
`judge-worker`。确认 Deployment 为 `Running/Healthy`、`judge-sandbox-v1` HostConfig digest 无 drift、
`judge_control`/`storage_get` Binding 为 Active，且最近注册和心跳成功。B Worker 不直连 Redis；Redis consumer
只属于 A 机 Judge API 内部实现。

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

通过 Orchestrator 查看 B 节点 RuntimeReport、Deployment health evidence 与固定 HostConfig digest。Agent 的
`judge-sandbox-v1` 启动后检查会验证 nsjail、cgroup v2、工具链和 work/cache 可写性；失败时 Deployment 不会
提升为 Healthy。如果 nsjail 不可用，不要切换到假 runner，也不要用安装请求覆盖 capability/mount/security
option；修复节点或发布新的签名 runtime profile。

## Pending 任务恢复

如果 pending 任务卡住：

1. 检查 `judge-api` 队列状态。
2. 检查 Redis `XPENDING`。
3. 通过 Orchestrator 对对应 Deployment 执行一次 restart，并观察 Operation 与重新注册日志。
4. 如果任务超过 lease TTL 仍 pending，使用受支持的 worker 恢复路径认领它，或在一次性环境中运行 Redis
   恢复演练。
5. 在 submission 状态与 result stream 对账完成前，不要手动删除 stream 条目。

参考演练：

```bash
deploy/ops/redis-recovery-drill.sh
```

## 吊销 workload 调用权

在 Topology draft 中解除对应 requirement 的 ApiBinding，核对 diff 后 apply；卸载 consumer 或 rebind provider 也会
提升 credential generation。确认新的 TopologyStatus 为 `IN_SYNC`、Binding 为 `REVOKED/UNBOUND`，并用旧 token
验证 Gateway 已立即拒绝。不要直接改 Auth/Gateway 数据库，也不要为恢复调用签发共享 service/worker token。

Node 身份泄露时还要在 Orchestrator 吊销该 Node 证书、drain 节点并重新 enroll；Deployment JWT 与 Node mTLS
是两条独立身份链。

## 回滚

进行 operation 回滚：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL="$ORCHESTRATOR_URL" \
OJOS_ROLLBACK_OPERATION_ID="$OPERATION_ID" \
OJOS_CONFIRM_ROLLBACK="rollback-$OPERATION_ID" \
OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1 \
deploy/ops/rollback-drill.sh
```

`OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1` 会授权执行固定的本地进程或 Compose 回滚动作。仅回滚 store 记录时
不要设置它。运行前先确认当前真实进程状态和运行资产齐全；运行后检查 HostService、Endpoint、
DeployedServiceApi、有效路由、权限和健康。schema 与外部资源副作用需按备份或专用补偿恢复。

脚本也支持按 Service 发起 `release.rollback`：设置 `OJOS_ROLLBACK_SERVICE`，再用
`OJOS_ROLLBACK_TARGET_OPERATION_ID` 精确指定原安装，或用 `OJOS_ROLLBACK_RELEASE_VERSION` 限定版本。
Release 模式必须设置 `OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1`。它与 `OJOS_ROLLBACK_OPERATION_ID`
互斥，示例见 [运维脚本说明](../../deploy/ops/README.md)。

## Problem 对象 GC 的删除隔离窗口

生产 Release 将 `storage_delete` Binding 超时固定为 60 秒，并将
`OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE` 默认设为 10 分钟。Problem Service 启动时要求 claim lease
严格大于 DELETE 超时再加 60 秒隔离 grace；不满足时拒绝启用生产 GC。

GC 在最终引用检查后、发出条件 DELETE 前会用 claim token 续满一次 lease。随后如果进程崩溃或请求超时，
ledger 会继续保持 `DELETING`，记录重试也不会缩短现有 lease，发布方不能重新注册同一
content-addressed URI。只有 claim lease 到期后
其他 collector 才能重领；默认 10 分钟窗口覆盖
60 秒请求上限和额外 60 秒 grace，避免旧 DELETE 与后续重新上传发生重叠。调整 Binding 超时或
`OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE` 时必须保持这一严格不等式，不能仅依靠 SHA/size 条件删除，
因为相同内容重新上传后的身份仍然相同。

## 首管理员一次性引导

1. 在宿主机生成 32–512 字符的 URL-safe 随机 token，保存为非符号链接普通文件，依次执行 `chown 65532:65532 <file>` 与 `chmod 600 <file>`，使 owner 精确为 Auth 容器身份 `65532:65532`、mode 精确为 `0600`。该值不得复用 JWT、internal、admin、observability、workload、Contribution ACK 或服务 token。
2. 将宿主绝对路径写入 `AUTH_ADMIN_BOOTSTRAP_SECRET_FILE`。production Compose 会把它只读挂到 Auth 固定路径 `/run/secrets/ojos-auth-admin-bootstrap`；不要使用 inline `AUTH_ADMIN_BOOTSTRAP_SECRET`。
3. 运行 `OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh`，启动后通过公网 HTTPS Gateway 向 `/api/auth/bootstrap/admin` 提交一次请求，并以普通登录确认管理员权限。
4. 成功后立即从实际生产 Compose 部署中同时移除 `AUTH_ADMIN_BOOTSTRAP_SECRET_FILE` env 和该 bind mount，删除宿主 token 文件，然后执行 `docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml up -d --force-recreate auth-service`（实际部署若使用生成后的 Compose，修改该部署源）。
5. 再次请求 `/api/auth/bootstrap/admin`，必须得到 `404`。数据库中的 durable consumed marker 仍提供第二道防线，但 `409` 不能代替撤掉路由。

## 备份 / 恢复

整栈备份是一个有界停写窗口，不是在线逐组件复制。先在 Gateway/调度层停止新写入，暂停服务、Worker
和迁移执行器，再由独立命令证明外部写屏障仍有效。fence token 只用于绑定本次变更单或租约；manifest
仅保存其 SHA-256。脚本在私有临时目录中生成并验证五个 PostgreSQL dump、Redis RDB、本地/对象存储
清单和精确 checksum 集，最后才原子发布备份目录。

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_BACKUP_SOURCE_ID=production-primary \
OJOS_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID='<applied stable service instance id>' \
OJOS_PROBLEM_RETAINED_VOLUME_NAME='<Agent-derived ojos-retain-* name>' \
OJOS_CONFIRM_QUIESCED_BACKUP=backup-production-fenced-v1 \
OJOS_BACKUP_FENCE_TOKEN="$CHANGE_AND_FENCE_ID" \
OJOS_BACKUP_FENCE_CHECK_COMMAND='/usr/local/sbin/ojos-fence-check' \
deploy/ops/backup.sh
```

先做无副作用校验：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/<stamp> \
OJOS_RESTORE_SOURCE_ID=production-primary \
OJOS_RESTORE_VERIFY_ONLY=1 \
deploy/ops/restore.sh
```

正式恢复前，在替换节点上以备份 identity 中相同的 stable owner instance 创建/核验一个空的 Agent-owned
Problem RETAIN volume；设置 `OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID`、
`OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_NAME`、`OJOS_RESTORE_RETAINED_VOLUME_TARGET_ID` 和
`OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER`。脚本拒绝 foreign labels、错误派生名、symlink mountpoint、非空树或
运行中挂载；只从已校验 SHA-256 的私有 staging archive 解包，逐文件 inventory 对账后才写入目标，并在结束时
重新 inspect/no-writer/fence。该路径覆盖旧节点或旧 volume 完全丢失的场景，不依赖节点保留。

正式恢复只允许写入一个仍被 fence 且所有组件为空/不存在的新环境；同源原地覆盖会被拒绝。默认在组件
验证后保持隔离，不切流。如需自动切流，必须成对提供 `OJOS_RESTORE_CUTOVER_COMMAND`、
`OJOS_RESTORE_ROLLBACK_COMMAND`，并提供 `OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND`；切换后检查失败会
立即回切旧环境。还必须提供 `OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND` 独立确认旧环境已重新承载流量；
回切无法确认时脚本禁止清理目标。Redis/local storage 恢复需显式指定服务 UID/GID 所有权。重新放量前仍需
运行预检、逐服务冒烟和业务对象读回，旧环境和备份在验收期结束前不得清理。

季度恢复演练必须使用全新五数据库、Redis 和对象存储命名空间，记录 manifest digest、组件探针、RPO/RTO
以及切流/回切结果。`OJOS_RESTORE_FAILPOINT` 只可在隔离演练中验证 `after-databases`、`after-redis`、
`after-storage`、`after-retained-volume`、`after-components` 边界。`deploy/ops/tests/full-stack-backup-restore-drill.sh` 提供本地存储
profile 的 clean-target 基准演练；它还把 Problem live tree、未决 mutation journal、数据库 outbox 与 immutable
artifact reference 一起恢复并对账。每次运行必须重新创建脚本要求的专用 target 数据库。MinIO profile 仍须在
独立命名空间执行同样的 manifest inventory 与对象读回验收。

## Trace

运行 trace 演练：

```bash
deploy/ops/trace-e2e-drill.sh
```

查询 Jaeger：

```bash
curl -fsS "$JAEGER_QUERY_URL/api/traces/$TRACE_ID" | jq .
```

预期服务包括 gateway-service、judge-api-service、storage-service 和 judge-worker。B Worker 的业务 span 只经
Gateway；Redis Stream relay/consumer span 属于 A 机 Problem/Judge API 的 outbox/inbox 投影，不应出现在 Worker
到 Redis 的直连链路中。

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
- `ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM` 不是 `1`：生产环境启用 release checksum 强制校验。

修改密钥后，重启受影响的服务并重跑预检。
