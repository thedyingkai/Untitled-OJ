# 生产运维脚本

本目录包含整套 OJ 服务和 Orchestrator v1 的预检、备份恢复及演练工具。脚本存在不代表门禁
已经通过；GA 证据必须来自候选 commit 对应的 CI run 和生产 profile 报告。

## Orchestrator v1 上线前检查

从仓库根目录的 `.env.production.example` 准备真实环境文件，然后运行：

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
bash deploy/ops/orchestrator-preflight.sh
```

该预检要求生产 PostgreSQL TLS、OIDC、控制面 HTTPS、Node CA、可信 Catalog、durable
artifact 目录、Web build、日志保留期和单主动控制面配置；缺项时 fail closed。整套 OJ Compose
的 secret、sandbox 与监控策略仍由 `preflight.sh`/`ci-policy.sh` 检查。

`deploy/compose/docker-compose.yml` 保留生产 daemon 的 fail-closed 行为。默认生产服务集会启动
`auth-db`、Auth migration、`auth-service` 和 `gateway` 作为保留的平台 bootstrap；Problem、
Judge、User、Storage 等业务服务仍只由 Catalog/Contribution 动态安装，不会回到静态 Compose。
Auth/Gateway 使用 `OJOS_PLATFORM_BOOTSTRAP=1`，它只表示平台自举，不等同
`OJOS_MANAGED_WORKLOAD=1`，因此不读取 Agent 的 `/run/ojos/.../context.json`。两者仍运行严格
production 校验，必须提供独立强凭据、Workload Ed25519 密钥和 Orchestrator TLS CA，且没有
development fallback。Gateway 的静态配置只保留 `/api/auth` 平台路由；业务路由、权限和前端
模块全部来自 Topology/Contribution 快照。

Compose 内部的 Orchestrator→Gateway/Auth 管理调用是唯一获准的明文例外：
`ORCHESTRATOR_ALLOW_COMPOSE_BOOTSTRAP_HTTP=1` 只接受精确的 `gateway`/`auth-service` DNS 名，
这些服务仅连接 `platform-control` internal network。非 Compose/capacity 部署必须提供 HTTPS
provider origins；缺少 provider pair 或 Auth workload issuer 时 Orchestrator 在监听端口前退出。
`deploy/compose/docker-compose.dev.yml` 是 trace/load 集成演练显式叠加的 ephemeral override，
只允许本地开发，不得作为生产配置，也不会恢复 0.2 Node push/bearer 路径。

生产 Compose 中六个证书/CA 变量填写宿主机绝对路径，Compose 会把它们只读挂载到固定的
`/run/secrets` 目标。`ORCHESTRATOR_MIGRATION_DATABASE_URL` 在迁移容器内使用，必须带
`sslmode=verify-full&sslrootcert=/run/secrets/orchestrator-postgres-ca.crt`；daemon 则通过
`ORCHESTRATOR_POSTGRES_CA_CERT` 使用同一 CA。控制面证书 SAN 需覆盖公开域名和内部服务名
`orchestrator`。私有 GitHub 下载优先使用固定透传的 `OJOS_GITHUB_TOKEN`，其次是
`GITHUB_TOKEN`；Catalog 的自定义 `auth_secret_ref` 必须通过一个最小 Compose override
显式注入对应变量，不要把整份生产 `env_file` 暴露给 daemon。

## Orchestrator 备份与恢复

备份必须在 daemon drain/停止后执行，并同时覆盖 PostgreSQL 与 artifact 目录：

```bash
export ORCHESTRATOR_DATABASE_URL='postgresql://...?...&sslmode=require'
export ORCHESTRATOR_ARTIFACT_DIR=/var/lib/ojos/orchestrator/artifacts
export ORCHESTRATOR_BACKUP_DIR=/srv/backup/ojos-orchestrator
export ORCHESTRATOR_HEALTH_URL=https://orchestrator.example.com
export ORCHESTRATOR_CONFIRM_QUIESCED_BACKUP=backup-orchestrator-v1
export ORCHESTRATOR_BACKUP_FENCE_TOKEN="$CHANGE_AND_FENCE_ID"
export ORCHESTRATOR_BACKUP_FENCE_CHECK_COMMAND='/usr/local/sbin/orchestrator-fence-check'
bash deploy/ops/orchestrator-backup.sh
```

恢复会校验 checksum 与 artifact inventory，先保留数据库和 artifact 前态，再原子切换 artifact 并在单事务中
恢复数据库；后置结构检查失败时会对称回灌两者。目标 daemon 必须停止：

```bash
export ORCHESTRATOR_DATABASE_URL='postgresql://...?...&sslmode=require'
export ORCHESTRATOR_ARTIFACT_DIR=/var/lib/ojos/orchestrator/artifacts
export ORCHESTRATOR_RESTORE_DIR=/srv/backup/ojos-orchestrator/20260803T010000Z
export ORCHESTRATOR_HEALTH_URL=https://orchestrator.example.com
export ORCHESTRATOR_CONFIRM_RESTORE=restore-orchestrator-v1
bash deploy/ops/orchestrator-restore.sh
bash deploy/ops/orchestrator-preflight.sh
```

恢复成功后仅启动一个控制面，确认 `/api/v1/healthz/ready` 返回 200，并核对 Node、Deployment、
Topology revision/head/status、Operation/Job/Event、Catalog、artifact 与审计后再开放流量。
Node 本地 execution ledger 不由控制面备份覆盖。

## 整栈 clean-target 恢复

`backup.sh`/`restore.sh` 覆盖五个 PostgreSQL 数据库、Redis、本地/MinIO 存储，以及
`problem-service` 的 `/data/ojos/problems` RETAIN managed volume。该 volume 是强制组件：生产备份从
`docker volume inspect` 核验 Agent 写入的 stable owner/service/logical-name/lifecycle/target 标签、派生 volume
name 与真实 mountpoint，并拒绝仍被运行中容器挂载的写者；随后保存 exact file inventory、tree digest、archive
与稳定 identity。节点丢失时，恢复到新节点上由同一 stable owner identity 预配的空 Agent-owned volume，
而不是假设旧节点仍存在。

备份要求外部写 fence，
在同一 backup root 的私有临时目录中完成组件验证和完整 manifest/checksum 后才原子发布。恢复默认仅执行
严格校验或写入一个与 source ID 不同、所有组件均为空/不存在的隔离环境；禁止直接清理或覆盖现有环境。

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/20260803T010000Z \
OJOS_RESTORE_SOURCE_ID=production-primary \
OJOS_RESTORE_VERIFY_ONLY=1 \
bash deploy/ops/restore.sh
```

正式 clean-target restore 还需目标 ID、clean-target 确认和独立 fence。默认完成后不切流；自动切流必须同时
配置 cutover、rollback 与 post-cutover check，后置检查失败会触发回切。完整操作顺序、故障注入边界和季度
演练要求见 `docs/ops/ops-runbook.md`。`tests/full-stack-backup-restore-drill.sh` 会真实调用这两个生产脚本，
且只接受五个专用 source/target 数据库、带环境哨兵的专用 Redis 和 run-scoped 本地存储；它不删除演练数据，
也不自动切流。整栈工具不替代上述 Orchestrator v1 专用脚本。

含 Redis、本地存储或 Problem RETAIN volume 的正式恢复必须分别设置 `OJOS_RESTORE_REDIS_OWNER=USER:GROUP`、
`OJOS_RESTORE_STORAGE_OWNER=USER:GROUP` 与 `OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER=USER:GROUP`；
retained volume 还要求 target ID、stable owner instance ID 和精确 Docker volume name。脚本只写入 identity
一致且为空、未被运行中容器挂载的目标，并在 copy 后重验 exact inventory、identity 与 fence。自动切流还必须配置
`OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND`，只有回切与独立回切检查均通过后才允许执行可选的失败目标清理。
MinIO 恢复必须显式设置 `OJOS_RESTORE_MINIO_TARGET_ID` 且与 target ID 完全一致，避免把 source endpoint 当成
clean target。
`OJOS_BACKUP_DIR` 必须预先创建；正式恢复中 `OJOS_RESTORE_WORK_ROOT`、`OJOS_RESTORE_EVIDENCE_DIR` 与各存储
target 的父目录也必须预先创建为彼此不重叠的真实目录。脚本拒绝符号链接和祖先/子目录重叠。

## 功能与容量门禁

- `orchestrator-docker-agent-e2e.sh`：真实 registry、Docker Engine、Node Agent 与 Store Job
  install/start/stop/restart/uninstall 生命周期；
- GA `release.yml` 升级门禁：由提取的 0.2 `PgOrchestratorStore` 向真实旧表写入 snapshot/runtime，
  再由 v1 CA 验证 TLS 仓储执行一次性导入；必须得到未应用 draft、`External/Unknown`
  runtime，且重开不得重复创建 revision 或 runtime；
- `orchestrator-backup-restore-drill.sh`：使用与服务端相同大版本的 PostgreSQL 客户端，真实验证
  数据库与 artifact 的联合备份、篡改后恢复、checksum、必需表和恢复前 artifact 保留；
- `orchestrator-capacity-gate.py`：100 Nodes、2,000 Deployments、10,000 Endpoint+Link、
  50 并发 Operation、重启恢复和 24 小时 soak；
- `validate-orchestrator-ga-evidence.py`：验证报告 commit、profile、规模、时长和阈值，拒绝手填或
  其他 commit 的证据；
- Web Playwright 与 TUI contract tests：正式 action、错误、默认值、cursor、ETag、SSE 和
  Idempotency-Key 等价；
- `trace-e2e-drill.sh`、`basic-load-soak.sh`：整套 OJ 的短时 trace/load 演练，默认显式叠加
  local dev Compose override，不构成 Orchestrator GA 容量证据。

`manager-smoke.sh`、`staging-drill.sh` 和 `rollback-drill.sh` 仍服务于旧整栈/0.2 兼容路径，
不得被用作 Orchestrator v1 GA 门禁。v1 rollback 通过正式 Store/Topology/Operation API 和
Node pull Job 执行，不接受旧 driver 授权变量。

## 其他整栈演练

- `service-credential-drill.sh`：服务凭据 allow/deny/revoke/expire；
- `redis-recovery-drill.sh`：Redis Stream pending claim/recovery 与 AOF 重启；
- `alert-firing-drill.sh`：Prometheus 规则和 Alertmanager webhook；
- `trace-e2e-drill.sh`：提交判题任务并核对跨服务 trace；
- `basic-load-soak.sh`：登录、题目、对象存储、判题和结果查询的短时冒烟。

监控栈可用真实生产 env 启动：

`ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE` and
`PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE` must be separate `0600`
files owned by the two non-root containers and containing the same dedicated
token. Never reuse an internal, admin, workload, or Contribution ACK token.
HTTP-SD publishes only ACTIVE Contribution heads backed by current HEALTHY
runtime evidence. Configure Gateway with an explicit external HTTPS
`ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN`, never a legacy Compose DNS name.

```bash
docker compose \
  --env-file /etc/ojos/production.env \
  -f deploy/ops/monitoring/docker-compose.yml \
  up -d
```
