# Orchestrator v1.0 数据库

Orchestrator 只写自己的控制面数据库，不直接写 auth、problem、judge、user 等业务服务的数据库。业务服务也不得直接修改 Orchestrator 表；migration 和 provider 通过各自受控接口或一次性签名任务执行。

## 三种存储后端

| 后端 | 用途 | 启动语义 |
| --- | --- | --- |
| Memory | 单元/契约测试和显式 `--ephemeral` 开发 | 永不作为 Desktop 或生产 daemon 的隐式回退。 |
| SQLite | Desktop 默认持久化 | 位于 OS 应用数据目录，启用 WAL、foreign keys、5 秒 busy timeout；数据库旁文件锁阻止两个 Desktop 同时写同一数据目录。打开、迁移或加锁失败即停止启动。 |
| PostgreSQL | 远程生产控制面 | `ORCHESTRATOR_DATABASE_URL` 必填，使用 r2d2 连接池和证书校验 TLS；迁移/checksum/readiness 或单主动 advisory lock 失败时，在绑定服务端口前退出。 |

Desktop 同一数据目录还保存 OCI artifact，但不保存 Agent execution ledger。ledger 位于独立 Node Agent 的私有持久根，是 Node 幂等执行的独立真值，不能由控制面数据库恢复覆盖。

## PostgreSQL 连接与单主动所有权

`PostgresPool` 使用 `r2d2_postgres` 与 rustls：

- 默认 pool 上限 16、最小 idle 2；连接 checkout 会验证可用性；
- 默认连接超时 5 秒、statement timeout 30 秒、lock timeout 5 秒、idle-in-transaction timeout 30 秒；
- 使用平台信任根，或由 `ORCHESTRATOR_POSTGRES_CA_CERT` 固定私有 CA；生产 URL 必须要求 TLS；
- readiness 报告数据库版本、TLS、recovery 状态、pool 状态和 schema 版本，不包含凭据；
- daemon 用一条专用 pooled connection 持有 session advisory lock。第二个控制面无法取得锁时 fail closed，不得绕过或退回 Memory。

该实现不再使用每次操作新建的 `postgres::Client + NoTls`，也不在写后全表回读并重建内存镜像。

## Schema 与迁移

SQLite 和 PostgreSQL 共享相同的逻辑迁移序列。`orchestrator_schema_migrations` 记录 version、name 和 checksum；已应用版本 checksum 不一致、数据库版本高于二进制、或必需对象缺失都会使 readiness 失败。

v1 持久对象按职责分组：

- `orchestrator_records` 与 `orchestrator_operation_logs_v2`：兼容领域记录和有序 Operation 日志；
- `orchestrator_jobs`、`orchestrator_job_events`：Job 状态、lease/heartbeat、attempt 和去重事件；
- `orchestrator_topology_revisions`、`orchestrator_topology_heads`、`orchestrator_topology_status`：不可变 Spec 历史、draft/applied ownership 和真实 Status；
- `orchestrator_runtime_instances`：Deployment/container、RepoDigest、desired/observed state 与 health；
- `orchestrator_idempotency`：mutation request fingerprint 与可重放响应；
- `orchestrator_durable_operations`：Operation 聚合和启动恢复索引；
- `orchestrator_audit_log`：append-only mutation intent/result；
- `orchestrator_node_enrollment_codes`、`orchestrator_node_certificates`：一次性注册码和 Node 证书生命周期；
- `orchestrator_legacy_imports`：0.2 → v1 一次性导入账本；
- `orchestrator_state`：schema/恢复所需的少量控制状态。

旧 `services/orchestrator/migrations/000001_orchestrator_schema.up.sql` 中的 normalized Service/Endpoint/Link/Topology 表属于 0.2 兼容输入，不是 v1 Topology/Runtime 的正式真值。

## 事务和并发规则

- 数据库事务只包裹状态读取、CAS、投影写入和审计；Catalog 下载、Docker、健康探测、Gateway/Auth 和其他 provider I/O 必须在事务外。
- Job claim/heartbeat/complete 使用 lease owner/epoch 的 CAS；旧 lease、重复 complete 和乱序 event 不能覆盖新 attempt。
- Topology draft/revision 使用强 ETag；PostgreSQL 通过行锁/事务 advisory lock、SQLite 通过 immediate transaction 保证单调 revision。
- mutation 在执行外部副作用前写 append-only audit intent；审计失败则 mutation 失败。
- retention 只清理已经终结的详细 Operation 日志、Job event 和过期幂等响应；Operation/Job 资源、Topology 历史和 audit 不由该任务删除。

## 0.2 数据导入

v1 第一次打开包含旧 normalized 表的 PostgreSQL 数据库时执行 expand-only、一致且幂等的导入：

- 旧 topology snapshot 转成未应用 draft revision，不推进 applied head；
- 旧 HostService 只能映射为 `External/Unknown` RuntimeInstance，不伪造 container ID 或 RepoDigest；
- `orchestrator_legacy_imports` 记录一次性结果，重启不会创建第二份 revision/runtime；
- 原旧表不由 v1 破坏性删除，便于备份验证和失败恢复。

真实升级的证据要求见 [生产就绪证据](../production-readiness.md)，备份/恢复步骤见 [v1 运维手册](operations-v1.md)。
