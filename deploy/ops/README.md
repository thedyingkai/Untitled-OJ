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

`deploy/compose/docker-compose.yml` 保留生产 daemon 的 fail-closed 行为。
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
bash deploy/ops/orchestrator-backup.sh
```

恢复会校验 checksum、原子切换 artifact，再在单事务中恢复数据库。目标 daemon 必须停止：

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

`backup.sh`/`restore.sh` 是整套 OJ 服务的备份工具，不替代上述 Orchestrator v1 专用脚本。

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

```bash
docker compose \
  --env-file /etc/ojos/production.env \
  -f deploy/ops/monitoring/docker-compose.yml \
  up -d
```
