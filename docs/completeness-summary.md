# Orchestrator v1.0 当前状态

本文描述当前源码已经实现的功能边界。源码中的版本号已经统一为 `1.0.0`；本地功能与 unsigned portable 交付按普通 CI 和安装 smoke 判定为 GO。生产规模报告、MSI 和代码签名是按实际发行需求选择的额外证据，不再作为功能完成条件。

历史 `v0.1.0-alpha` 使用旧原生 GUI，不代表当前 Desktop、Store、Topology 或 Agent 的实现。

## 已完成的 v1 功能面

| 模块 | 当前实现 |
| --- | --- |
| `orchestrator-core` | 纯领域模型、校验、确定性 plan/diff、状态机和 published action；不持有数据库、文件、网络、进程或 Docker I/O。 |
| `orchestrator-storage` | Memory、SQLite、PostgreSQL 三后端契约；SQLite 使用 WAL、外键、busy timeout 和数据库旁文件锁；PostgreSQL 使用证书校验 TLS、连接池、schema checksum 和专用 advisory-lock 连接。 |
| `orchestrator-control-plane` | 持久 Operation/Job、attempt/event、lease CAS、心跳、重试、取消、恢复、补偿和 reconciler。 |
| `orchestrator-runtime` / `orchestrator-agent` | Node 通过 mTLS pull 协议领取固定任务，使用本地 SQLite 执行账本，并通过 Docker Engine Unix socket 或 Windows named pipe 执行；不接受任意 shell。 |
| `orchestrator-manager` | Catalog v2、Release 导入/校验、依赖与平台选择、Store 安装/升级/回滚/卸载及类型化 provider 编排。 |
| Topology | `TopologySpec`、不可变 Revision、确定性 diff、validate/apply/rollback saga、正式 Status、drift 和按用户保存的 UI layout。 |
| backend | 唯一 `/api/v1` 契约、Problem Details、request id、Idempotency-Key、cursor、ETag/If-Match、SSE、OIDC/RBAC、append-only 审计、live/ready、指标和有界过载。 |
| Desktop / Web / TUI | Tauri WebView 内嵌 backend 与 loopback Agent；远程 Web 使用 OIDC Code + PKCE；TUI 是 OIDC Device Flow 的 `/api/v1` 客户端。Web/TUI published action 矩阵由契约测试锁定。 |

0.2 Console、旧 PostgreSQL 仓储、Docker Compose/本地进程适配和旧路由被隔离在 `orchestrator-legacy`，只服务兼容构建与迁移，不是 v1 生产路径。

## 正式运行形态

### Desktop

- 默认在操作系统应用数据目录创建 SQLite、Agent ledger 和 artifact 目录，跨重启保存 Store、Topology、Operation、Job 和布局。
- 在随机 loopback 端口启动 embedded backend 与 loopback Agent，由 Tauri 原生 WebView 加载同源页面，不打开外部浏览器。
- 一次性 bootstrap secret 只用于兑换 HttpOnly 本地 admin 会话；SQLite 打开失败时不会回退内存。

### 远程控制面

- 生产形态固定为单主动 daemon + PostgreSQL，最多 100 个 Node；PostgreSQL、TLS、OIDC、Node CA、可信 Catalog、Web build 或 durable artifact 目录缺失时 fail closed。
- 单主动所有权由专用 PostgreSQL advisory-lock 连接保持。内存后端只允许显式 `--ephemeral` 的开发和测试。
- SIGTERM 停止接收新工作并最多排空 30 秒；重启先校验 schema/readiness 并恢复过期 Job/Operation，再进入 ready。

## Store 与 Topology 状态

Store 已按 Catalog v2 与运行时投影工作：生产 Catalog 使用 RFC 8785 规范化 JSON、Ed25519 信任和 OCI digest；安装默认 `start=true`，目标由 `target_node_id` 明确指定。Managed 安装经持久 Job 拉取、创建、启动和健康验证后才能投影为 Running；External 安装必须验证 endpoint。失败会保留 imported Release，但不会伪报 installed/running，并对已经产生的副作用执行补偿或进入 `NEEDS_ATTENTION`。

Topology 不再是观察快照。Endpoint/Link 编辑产生 draft revision，apply 才创建异步 Operation；applied head 只在必要步骤全部成功后推进。Rollback 复制旧 Spec 生成新 revision，再执行正常 apply。Web/TUI 的健康和 drift 来自 `TopologyStatus`，不从 Spec 或画布字段推断。

类型化 Gateway/Auth、migration、config/secret、Redis、storage、frontend 与 API registry provider 已进入 Release pipeline。缺少计划所需 provider 时会在外部副作用前拒绝，不返回 deferred、skipped 或假成功。

## 契约与兼容边界

- 正式 API 前缀为 `/api/v1`；成功响应包含 `request_id`，失败使用 `application/problem+json`。
- 长操作返回 `202 + operation_id`；所有 mutation 要求 `Idempotency-Key`；Revision 使用强 ETag，Operation 日志/事件支持 SSE 重连。
- Web 与 TUI 只展示 published capabilities。发布 action 矩阵要求零 `UNSUPPORTED`。
- `0.2.0` 兼容构建保留带弃用头的旧路由；`1.0.0` 对旧 mutation 返回 `410 Gone`，旧 Node push/shared-bearer 路径不存在。

## 可选的生产规模与签名证据

以下事项只能由候选环境、长时运行或发布系统提供，也不应写成功能缺口：

1. **同一候选 commit 的生产规模与 24 小时证据**：在直接连接单主动控制面的环境中验证至少 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operations，以及读 p95 ≤ 200 ms、异步 mutation 接受 p95 ≤ 500 ms、SSE 事件 p95 ≤ 1 秒、真实重启恢复 ≤ 60 秒、RSS 增长 < 10% 和无永久运行任务。报告必须由 production profile 生成，并与候选 commit 精确绑定。
2. **签名的多平台 GA 制品**：发布 workflow 已定义 Windows x64 MSI/portable ZIP 与 Linux x86_64 DEB/AppImage/tar.gz、SHA256、SPDX SBOM、provenance 和 Sigstore bundle 的构建与校验，但尚不能把 workflow 定义当成已生成、已验证或已发布的 GA 制品。

只有在项目决定对外声明 100 Node 容量或发布受信任签名安装包时，才需要执行对应流程。普通本地交付使用 unsigned portable 包内的原生 `ojos-orchestrator install`；仓库没有 `sh`、`bat/cmd` 或 `ps1` 产品安装入口。详细执行方式见 [v1 运维手册](orchestrator/operations-v1.md) 和 [生产就绪证据](production-readiness.md)。

真实升级/恢复已经从外部缺口转为发布功能门禁：0.2 历史仓储 writer 在 TLS PostgreSQL 17
写入旧 schema，v1 验证一次性导入、未应用 draft、`External/Unknown` runtime 和重启幂等；
另有 PostgreSQL/artifact 联合备份恢复 drill。两者已在本地真实 PG17 环境通过，并由
`release.yml` 在候选 commit 上自动重跑。

## 明确不属于 v1 GA 缺口

v1 固定不实现 active-active HA、自动扩缩容、通用调度器、Kubernetes runtime、多租户计费或任意本地命令执行。远程部署的“单主动”是产品边界，不应被误写成未完成项。
