# Orchestrator v1.0 运维手册

本文只描述 v1 控制面的部署、观测、备份、恢复和容量门禁。生产形态是单主动 daemon + PostgreSQL，Node 通过 mTLS 拉取任务；Desktop 使用本地 SQLite，不使用本手册的远程部署配置。

## 启动前门禁

构建 Web UI 后，先执行：

```bash
npm --prefix manager/web ci
npm --prefix manager/web run build
bash deploy/ops/orchestrator-preflight.sh
```

生产环境必须同时满足：

- `ORCHESTRATOR_DATABASE_URL` 指向 PostgreSQL，并显式使用 `sslmode=require`；`ORCHESTRATOR_POSTGRES_CA_CERT` 必须指向验证该服务的 CA。Compose 的 `orchestrator-migrations` 使用单独的 `ORCHESTRATOR_MIGRATION_DATABASE_URL`，该 URL 必须包含 `sslmode=verify-full&sslrootcert=/run/secrets/orchestrator-postgres-ca.crt`，迁移与 daemon 共用同一个只读 CA 挂载。
- 配置 `ORCHESTRATOR_OIDC_ISSUER`、`ORCHESTRATOR_OIDC_AUDIENCE`、`ORCHESTRATOR_OIDC_CLIENT_ID` 和 `ORCHESTRATOR_PUBLIC_BASE_URL`。Issuer、公开地址必须是 HTTPS。
- 配置 `ORCHESTRATOR_TLS_CERT`、`ORCHESTRATOR_TLS_KEY`、`ORCHESTRATOR_NODE_CA_CERT` 和 `ORCHESTRATOR_NODE_CA_KEY`。Node CA 私钥只提供给控制面进程。Compose 内部 readiness 直连 `https://orchestrator:8090`，因此服务端证书 SAN 必须同时覆盖公开域名和 Compose 服务名 `orchestrator`。
- `ORCHESTRATOR_CATALOG_TRUST_KEYS` 和 `ORCHESTRATOR_CATALOG_SOURCES` 均为非空 JSON；daemon 启动时会加载并验证来源，Store 不接受任意 URL 导入。
- Gateway/Auth Provider 要么成对配置，要么都不配置。未配置时 daemon 可以提供只读功能，但相关 Store/Topology plan 会明确失败，不会生成 deferred 或假成功。
- `manager/web/dist/index.html` 必须存在。生产 daemon 不会退回占位页面或默认内存存储。

daemon 在绑定端口前依次完成 PostgreSQL TLS/schema checksum/readiness、单主动 advisory lock、OIDC discovery/JWKS、服务端证书和 Node CA、Catalog bootstrap、过期 Job/Operation 恢复。任一环节失败都会退出。

私有 GitHub Catalog/元数据包下载默认优先读取 `OJOS_GITHUB_TOKEN`，其次读取 `GITHUB_TOKEN`，Compose 只透传这两个固定变量。Catalog source 若使用 `auth_secret_ref: env:CATALOG_OFFICIAL_TOKEN` 之类的自定义变量，需要在独立的生产 Compose override 中只注入该变量：

```yaml
services:
  orchestrator:
    environment:
      CATALOG_OFFICIAL_TOKEN: ${CATALOG_OFFICIAL_TOKEN:?set Catalog credential}
```

不要把整份生产 `env_file` 挂给 daemon；它会把与编排器无关的数据库、对象存储和服务凭据一起扩大到进程边界之外。

## Standalone Node Agent

Store 类型化 Provider 的 Node 广告与 Agent 本地配置必须使用同一个 ID：`providers.redis.connection_id` 对应 `ORCHESTRATOR_REDIS_CONNECTIONS_JSON/FILE` 的键，`providers.storage.connection_id` 对应 `ORCHESTRATOR_STORAGE_CONNECTIONS_JSON/FILE`，`providers.frontend.asset_store_id` 对应 `ORCHESTRATOR_FRONTEND_ASSET_STORES_JSON/FILE`，`providers.api_registry.registry_id` 对应 `ORCHESTRATOR_API_REGISTRIES_JSON/FILE`。ID 不一致时 plan 或 Agent 执行会明确失败，不会回退为成功；通用 HTTP provisioner 仅在显式设置 `ORCHESTRATOR_ENABLE_EXTERNAL_PROVISIONER_FALLBACK=true` 时启用。Auth/Gateway 同时接受现有的 `ORCHESTRATOR_*_ADMIN_ORIGIN` 和 `ORCHESTRATOR_*_ADMIN_ENDPOINT`，两者并存时 `ENDPOINT` 优先。

Catalog 的 `runtime_capabilities` 是 release-version 级签名字段。`link-probe-v1` 只有在同一 metadata manifest 也精确声明 `orchestrator.link-probe.v1` 时有效；旧 Catalog 缺少字段视为不支持。若 Catalog 与 metadata 不一致，检查 Catalog 生成器和 metadata 包并重新签名，不能修改数据库投影绕过 `CATALOG_METADATA_CAPABILITY_MISMATCH`。

Managed Store endpoint 必须写成 `Node.host_ip:host_port:service_id`。运行时会生成唯一的 Docker `8080/tcp`（或该 release 签名 backend port）PortBinding，投影保留精确 endpoint 和 `release_version`。升级或回滚若继续使用正在运行的旧实例 host port，会返回 `STORE_REPLACEMENT_ENDPOINT_CUTOVER_UNSUPPORTED`；为新实例提供不同端口，待健康和切流完成后再回收旧实例。已有投影的 endpoint/release 不符时需要受控重新部署，不得把它当作可恢复的同一实例。

Topology validate/apply/rollback 会用 Endpoint+service、RuntimeInstance.release_version 和 ServiceRelease manifest 做精确绑定。启用 Link 的 source 必须唯一绑定且具备双重签名能力，否则返回 `TOPOLOGY_LINK_PROBE_RELEASE_BINDING_REQUIRED` 或 `TOPOLOGY_LINK_PROBE_CAPABILITY_REQUIRED`。reconciler 用固定 16 worker、每轮最多 512 Endpoint 和 1,024 Link，执行无重定向、750 ms 超时、4,096-byte 上限的真实 `/health` 与 source-side `/probe`；分批期间 Status 明确为 Unknown/Degraded，证据收齐后才可进入 `IN_SYNC`。历史 diff 不访问当前运行态，服务删除后仍可审计。

远程 Node 运行独立的 `ojos-orchestrator-agent`，通过 mTLS 长轮询领取只分配给本节点的持久 Job，并直接连接本机 Docker Engine。注册分成一次性 `enroll` 和长期 `run` 两步；不要把注册码直接写入 service 参数或环境变量。

先由 admin 在 Web 的 Node 页面创建一次性注册码，或调用 v1 API 后把响应中的 `enrollment_code` 写入权限受限的临时文件：

```bash
curl --fail-with-body \
  --request POST https://orchestrator.example.com/api/v1/nodes/enrollment-codes \
  --header 'Content-Type: application/json' \
  --header 'Idempotency-Key: 018f-node-enroll-0001' \
  --header "x-csrf-token: $ORCHESTRATOR_CSRF_TOKEN" \
  --cookie "$ORCHESTRATOR_SESSION_COOKIE" \
  --data '{"node_id":"worker-01","host_ip":"10.20.0.31","role":"worker","ttl_seconds":600}'
```

在目标 Node 上准备控制面 HTTPS CA、持久 identity 目录和注册码文件，然后只兑换一次：

```bash
install -d -m 0700 /var/lib/ojos-agent/identity
ojos-orchestrator-agent enroll \
  --control-plane https://orchestrator.example.com \
  --ca /etc/ojos/control-plane-ca.pem \
  --expected-node-id worker-01 \
  --identity-dir /var/lib/ojos-agent/identity \
  --enrollment-code-file /run/ojos/enrollment-code
```

成功时命令输出一行 JSON，包含 `node_id`、SPIFFE ID、证书 serial、`not_after_ms` 和 `renew_after_ms`。`--expected-node-id` 在预置环境中应始终传入：它把结果固定到该 Node，返回其他身份时命令失败；人工恢复省略该参数时，只接受所选 generation 自身证书中的精确 Node/SPIFFE 绑定。Agent 会在首次请求前，把与注册码摘要、控制面 origin、预期 Node（如有）和服务器 CA 摘要绑定的 CSR/私钥原子写入 identity 目录。若控制面已提交兑换但响应丢失，重试会复用字节完全相同的 CSR；同一 CSR 仅在原证书仍处于 ACTIVE 且有效期内时得到原证书，吊销、尚未生效、过期或不同 CSR 的重放都会被拒绝。

尝试新注册码不会销毁旧的未决 CSR/私钥：各注册码的完整 pending 尝试按摘要持久归档，切回旧注册码仍会恢复其原始密钥。只有某次身份通过控制面的只读 mTLS 账本校验、generation 被原子发布为 current 且无更新 generation 会被回退后，Agent 才写入不含私钥的 completed marker 并清理其他未决尝试。因此请求提交、响应丢失、generation 发布、在线校验或 current 更新任一边界掉电都可恢复；`ENROLLED`/`RECOVERED` 绝不只代表本地文件存在。

identity 目录保存版本化证书、私钥、控制面 CA、注册恢复状态与当前 generation 指针；它必须位于本地持久磁盘并只允许 Agent 服务账号访问。completed marker 只保存 CSR、绑定摘要和证书 serial，不保存私钥。只有命令成功或输出 `RECOVERED` 后才删除临时注册码文件；不要备份或把它复用到其他 Node。

前台验证：

```bash
ojos-orchestrator-agent run \
  --control-plane https://orchestrator.example.com \
  --identity-dir /var/lib/ojos-agent/identity
```

默认执行账本为 `/var/lib/ojos-agent/identity/execution-ledger.sqlite3`，可用 `--ledger` 指向同一节点上的其他持久路径。账本记录 claim/attempt/副作用结果，用于至少一次投递下的幂等恢复；不得放在临时目录，也不得用控制面数据库备份覆盖。默认 heartbeat 为 10 秒、lease 为 30 秒、传输重试为 1 秒。

私有 OCI Registry 使用 `--registry-credentials /etc/ojos/agent/registry-credentials.json` 显式启用。文件采用严格 schema v1：`{"schema_version":1,"registries":[{"server_address":"ghcr.io","username":"...","password":"..."}]}`；最多 32 个 Registry、文件最大 64 KiB，拒绝未知字段、重复 host、URL、浮动 tag 和跨 Registry 复用。Agent 只把匹配目标 digest 引用 host 的凭据交给 Docker Engine API，不写入 Job、Operation 或日志。该文件必须由服务管理器以只读方式物化；轮换后重启 Agent，让新 worker 重新加载凭据。

Linux systemd 服务示例：

```ini
[Unit]
Description=OJOS Orchestrator Node Agent
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service

[Service]
Type=simple
User=ojos-agent
Group=ojos-agent
ExecStart=/usr/local/bin/ojos-orchestrator-agent run --control-plane https://orchestrator.example.com --identity-dir /var/lib/ojos-agent/identity --registry-credentials /etc/ojos/agent/registry-credentials.json
Restart=on-failure
RestartSec=5s
TimeoutStopSec=35s
KillSignal=SIGTERM

[Install]
WantedBy=multi-user.target
```

服务账号必须能够访问 Docker Unix socket；Windows 服务账号必须能够访问 Docker named pipe，并把 identity/ledger 放在例如 `C:\ProgramData\OJOS\agent` 的持久且受 ACL 保护的目录。服务管理器应发送 SIGTERM/控制台停止事件并至少留出 30 秒排空时间，不能周期性删除 identity 或 ledger。

Node 证书默认有效 30 天，控制面把 `renew_after_ms` 设为到期前 7 天。`run` 会在该时间生成新 CSR，并用当前 mTLS 身份申请候选证书；签发阶段保留旧证书有效。Agent 先把候选证书与私钥完整写入新的版本化 generation，再用新证书调用 `/api/v1/agent/certificates:activate`，由控制面原子激活新 serial 并撤销旧 serial，最后重启 worker transport。签发、落盘、activate 任一响应丢失或进程崩溃都可按本地 generation 状态安全重试，不会留下“旧证书先撤销但新证书未落盘”的失联窗口。失败按 `--renewal-retry-ms`（默认 60 秒）重试；若已无法在过期前安全重试，或加载时证书已经过期，Agent 会失败退出并要求人工重新注册，不会降级到 bearer/push 路径。即时吊销后该 Node 的新 claim、heartbeat、complete、artifact 下载和续签都会被控制面拒绝。

## 健康、指标和日志

- `GET /api/v1/healthz/live`：进程存活，不访问业务锁。
- `GET /api/v1/healthz/ready`：验证持久存储、schema 和恢复结果；失败返回 `503` 与 `Retry-After`。
- `GET /metrics`：Prometheus text format，包含请求总量/延迟、活跃请求、过载拒绝、OTLP 丢弃、RSS、线程数，以及按状态统计的持久 Job、过期 lease、最旧 LEASED Job 心跳年龄和 Job 指标读取失败标志。它还输出持久化的过期 lease 转换、超过 300 秒 Operation、无效 Operation 时间戳、控制面启动次数，以及观测错误和状态加载标志；持久快照无法读取或计数 checkpoint 失败时返回 `503`，不把缺失值伪装成零。
- 所有 429/503 响应都带 `Retry-After`。连接队列满时不创建新工作线程。

stderr 每行是一条 JSON，`http_request_completed` 至少包含 `request_id`、method、无查询参数 path、低基数 route、status、duration、peer，以及响应中出现的 `operation_ids`、`job_ids`、`node_ids` 和业务状态。请求体、令牌、Cookie、Catalog 密钥和证书不会写日志。

可选 OTLP/HTTP：

```bash
export ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT=https://otel-collector.example.com:4318
export ORCHESTRATOR_OTEL_EXPORT_TIMEOUT_MS=2000
export ORCHESTRATOR_OTEL_QUEUE_CAPACITY=1024
```

未设置 endpoint 时不启动 exporter，也没有请求路径上的 channel/网络开销；设置后使用有界 `try_send`，Collector 故障只增加丢弃计数，不阻塞请求。

daemon 还要求 `ORCHESTRATOR_ARTIFACT_DIR` 指向持久、可写目录，用于离线 OCI archive 的分块交付与恢复；该目录必须和 PostgreSQL 一起纳入容量和备份策略，但不能由数据库恢复覆盖正在执行的 Agent 本地账本。生产连接池默认保留 160 个 HTTP worker（可用 `ORCHESTRATOR_MAX_WORKERS` 调大，不能低于 128），以容纳 100 个固定 25 秒 Agent 长轮询和 Web/API 流量。

`ORCHESTRATOR_LOG_RETENTION_DAYS` 默认 30 天，可配置 1–3650 天。daemon 启动时及之后每小时只清理已经终结的 Operation 详细日志、Job 事件和过期幂等响应；Operation/Job 资源本身、Topology 历史和 append-only 审计永不由该保留任务删除。清理查询失败会保留原数据并在下一轮重试。

日志保留交给进程管理器。systemd 推荐 `StandardOutput=journal`、`StandardError=journal`，并在 journald 设置 `SystemMaxUse`、`MaxRetentionSec`；容器运行时设置 `json-file` 的 `max-size`/`max-file`，或使用支持轮转的日志驱动。生产默认建议保留 14 天、单实例最多 10 GiB；审计记录位于数据库 append-only 表，不依赖 stderr 保留期。

## 停止与升级

SIGINT/SIGTERM 会立即停止接收新连接，唤醒 accept loop，然后排空队列和执行中的请求，最长 30 秒。超过期限 daemon 以错误退出；未能证明结果的 Job/Operation 会在下次启动按恢复规则进入重试或 `NEEDS_ATTENTION`，不会盲目重复外部副作用。

升级顺序：

1. 生成并校验备份。
2. 部署 `0.2.x` 兼容构建，运行旧路由迁移演练并确认弃用响应头。
3. 停止流量、发送 SIGTERM，确认进程在 30 秒内退出。
4. 启动新二进制；仅当 ready=200 后恢复流量。
5. 升级至 `1.0.0` 后，旧 mutation 返回 410；Node push/bearer 路径不再可用。

生产容量/24 小时 soak runner 还必须在专用 Runner.Listener 服务的受保护 `.env` 中预置唯一的 `ORCHESTRATOR_GATE_RESTART_ARGV_JSON`。它是无需 shell 的 JSON argv，argv0 必须是 Ansible 部署并纳入 helper manifest 的受限重启 wrapper；workflow 不从 GitHub Secret 注入第二份值。门禁直接访问单主动控制面 origin，先写入一个未应用的持久 Operation，再执行真实重启；只有观测到 readiness 中断、60 秒内恢复且同一 Operation 仍可读取时，恢复证据才有效。负载均衡地址不能用于这项门禁，因为它会掩盖控制面重启。

## 备份与恢复

备份同时覆盖 PostgreSQL 和 durable OCI artifact 目录，使用 PostgreSQL custom format、
压缩 artifact archive、内容清单和 SHA-256；默认保留 30 天。为了得到一致快照，先 drain/停止
daemon，再显式确认控制面已经 quiesced：

```bash
export ORCHESTRATOR_DATABASE_URL='postgresql://...?...&sslmode=require'
export ORCHESTRATOR_ARTIFACT_DIR=/var/lib/ojos/orchestrator/artifacts
export ORCHESTRATOR_BACKUP_DIR=/srv/backup/ojos-orchestrator
export ORCHESTRATOR_HEALTH_URL=https://orchestrator.example.com
export ORCHESTRATOR_CONFIRM_QUIESCED_BACKUP=backup-orchestrator-v1
bash deploy/ops/orchestrator-backup.sh
```

脚本在配置 health URL 时会拒绝备份仍存活的 daemon，并把数据库、artifact 文件数/字节数和
`control-plane-quiesced` 一致性声明写入 manifest。每日至少备份一次，恢复演练至少每月一次。
Node 本地执行账本不属于控制面备份；Node 重连后按幂等账本与服务端状态对账。

恢复必须先停止 daemon，并显式确认：

```bash
export ORCHESTRATOR_RESTORE_DIR=/srv/backup/ojos-orchestrator/20260803T010000Z
export ORCHESTRATOR_ARTIFACT_DIR=/var/lib/ojos/orchestrator/artifacts
export ORCHESTRATOR_CONFIRM_RESTORE=restore-orchestrator-v1
export ORCHESTRATOR_HEALTH_URL=https://orchestrator.example.com
bash deploy/ops/orchestrator-restore.sh
bash deploy/ops/orchestrator-preflight.sh
```

恢复脚本先校验全部 checksum，把 artifact 解压到同父目录的 staging 目录后原子切换，再在单个
PostgreSQL 事务中恢复数据库；数据库恢复失败时会把原 artifact 目录切回。恢复成功后原 artifact
目录以 `.before-restore.*` 保留，验证完成后由运维人员处理。随后只启动一个控制面，要求
ready=200，并抽查 Node、Deployment、Topology revision/head/status、Operation/Job/Event、
Catalog source、artifact 下载和审计记录，再开放流量。

## 容量与稳定性门禁

环境先准备 100 Nodes、2,000 Deployments，以及总计至少 10,000 个 Endpoint+Link。门禁自身并发创建 50 个无部署副作用的 health Operation plan，执行 confirm/apply/cancel，并检查 SSE 首事件。直接运行脚本只用于 smoke 和协议调试：

```bash
python3 deploy/ops/orchestrator-capacity-gate.py \
  --base-url https://orchestrator.example.com \
  --ca-file /etc/ojos/control-plane-ca.pem \
  --profile smoke \
  --soak-seconds 0 \
  --report artifacts/capacity-smoke.json
```

默认阈值是读 p95 200 ms、异步 mutation 接受 p95 500 ms、事件 p95 1 秒、恢复 ready 60 秒。smoke 只能验证 harness 与协议，不能替代生产规模证据。正式 production 不接受省略构建身份、动态 token helper、真实重启 helper、runner 连续性或环境 observer 的脚本直跑；唯一受支持入口是下面的首次 workflow dispatch。

它持续检查 ready、永久 RUNNING/ENQUEUING/CANCELLING Operation、过期 Job lease、停止心跳的 LEASED Job、RSS 与线程增长；暖机后 RSS 最大值不得超过基线 10%。24 小时任务应在受控 self-hosted runner 运行，失败时保留 JSON 报告、daemon JSON 日志和 Prometheus 时序。

production harness 不再使用宿主机 `/proc/uptime` 或 systemd wrapper 代替 runner 在线时间。它把 `RUNNER_NAME`、当前进程的 `actions.runner.*.service` 和 `systemctl show` 返回的 ControlGroup 严格绑定，再沿当前 Job 的 `/proc` PPID 链定位真实的 `Runner.Listener` 祖先进程。门禁只接受 workflow 首次运行；使用 Actions API 的 `Date` 与 workflow `created_at`，结合 unit 的 MONOTONIC 年龄和 Listener 的 BOOTTIME 启动 tick，分别证明两者在派发前已连续运行至少一小时，本机墙钟只做 30 秒偏差预检。基线、每个 30 秒采样和最终检查的 boot ID、ControlGroup、unit、InvocationID、MainPID、Listener PID/启动 tick 和观测进程身份必须完全一致。BOOTTIME 读数由两次 MONOTONIC 读数夹逼，普通调度延迟不会被误判为 suspend，而真实的 BOOTTIME-MONOTONIC offset 变化仍会使门禁失败。workflow token 仅授予 `actions: read`，不会写入报告。

GA release 不接受 smoke、容量-only、其他 commit 或手工填写的结论。候选 commit 必须先在带 `orchestrator-soak` label 的专用 self-hosted runner 上完成 `orchestrator-capacity.yml` 的 production 任务：

```bash
gh workflow run orchestrator-capacity.yml \
  --ref main \
  -f base_url=https://orchestrator-staging.example.com \
  -f profile=production \
  -f soak_seconds=86400 \
  -f candidate_image_run_id="$CANDIDATE_IMAGE_RUN_ID"
```

`CANDIDATE_IMAGE_RUN_ID` 必须是当前 `main` SHA 对应的
`orchestrator-candidate-images.yml` 成功首次运行；不得使用其他 commit、rerun 或手工拼出的镜像。完整的 run 身份复核、GitHub Environment 和 runner service 配置命令见
[`deploy/capacity/README.md`](../../deploy/capacity/README.md)。

报告会记录实际 checkout 的 `GITHUB_SHA`、经 Actions API 验证的派发时间、runner systemd 服务基线/逐样本/最终连续性证据、请求/实际 soak 秒数、采样数、规模、延迟以及 RSS/线程基线。`release.yml` 只查找同一 `github.sha` 的成功 production artifact，并重新验证 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operations、24 小时持续时间和全部阈值；找不到有效证据时 `ga-build` 不会开始。

## 故障判断

- ready=503：保持摘流，按 `detail` 检查数据库、schema checksum、连接池或恢复；遵守 `Retry-After`。
- 第二控制面启动失败：这是单主动锁的预期行为，不要绕过 advisory lock。
- Store/Topology plan 报 Provider missing：补齐对应 Provider，不能人工把 Operation 标成成功。
- Operation=`NEEDS_ATTENTION`：核对 Node 本地账本、Docker RepoDigest、Provider 实际状态和审计记录，再选择 retry/rollback。
- OTLP dropped 增长：修复 Collector 或扩大有界队列；不要把 exporter 改成同步。
- RSS/线程持续增长或 24 小时 gate 失败：停止 GA，保留报告并定位泄漏后重新从零运行完整 soak。
