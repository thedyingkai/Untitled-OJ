# OJOS Orchestrator

OJOS Orchestrator 是 OJOS 的服务控制面。它负责可信 Catalog/Release、显式节点放置、Deployment 生命周期、版本化 Topology、持久 Operation/Job、观测和诊断；题目、提交、判题、用户、比赛等业务仍由各 Service 实现。

当前源码版本为 `1.0.0` 发布候选，**尚未发布 GA**。剩余放行条件见[发布候选判定](docs/release-candidate.md)。

## v1 架构

```text
Desktop / Web / TUI
        │ /api/v1 + SSE
        ▼
single-active control plane
        ├─ pure core + manager + control-plane
        ├─ SQLite (Desktop) / PostgreSQL (remote production)
        └─ persistent Operation / Job / Topology / Runtime projections
                              │ mTLS pull
                              ▼
                       per-Node Agent
                       local SQLite ledger
                       Docker Engine API
```

主要对象：

- `CatalogSource` / `ServiceRelease`：带版本、平台、依赖、校验、OCI digest 和签名的发布输入。
- `Deployment` / `RuntimeInstance`：明确目标 Node 的运行实例、container、实际 RepoDigest、期望/观测状态和健康。
- `TopologySpec` / `TopologyRevision` / `TopologyStatus`：期望 Endpoint/Link、不可变历史、实际健康与 drift。
- `Operation` / `Job`：计划、确认、异步执行、lease、重试、补偿、日志和恢复。
- `Node`：使用 SPIFFE Node ID mTLS 证书领取持久 Job 的独立 Agent。
- `Diagnostic`：面向 Deployment、Topology 和 Operation 的只读诊断结果。

Store 负责安装和节点放置；Topology 只连接已经注册或部署的服务，不会隐式安装服务。

## 正式入口

- **Desktop**（`manager/desktop`）：默认入口。在 Tauri 原生 WebView 中加载同源 Web UI，并启动随机 loopback backend 与 loopback Agent；默认使用 OS 应用数据目录中的 SQLite，不打开外部浏览器。
- **远程 Web**（`manager/web`）：由生产 daemon 托管，使用 OIDC Authorization Code + PKCE 和 HttpOnly 会话。
- **TUI**（`manager/tui`）：远程 `/api/v1` 客户端，使用 OIDC Device Authorization Grant，不在进程内执行 mutation。
- **daemon**（`services/orchestrator/backend`）：单一 REST/SSE 控制面。生产必须使用 PostgreSQL、TLS、OIDC、Node CA、可信 Catalog 和 durable artifact 目录。

Web 与 TUI 使用相同的 published action、RBAC、默认值、Problem Details、cursor、ETag、SSE 和 Idempotency-Key 语义。TUI 不复刻拖拽画布，但控制能力一致。

## 本地开发

Node.js 需要 `^22.18.0` 或 `>=24.11.0`：

```bash
npm --prefix manager/web ci
npm --prefix manager/web run typecheck
npm --prefix manager/web test
npm --prefix manager/web run build

# 默认图形入口：持久 SQLite + embedded backend/Agent + WebView。
cargo run -p ojos-orchestrator-desktop
```

远程 TUI 示例：

```bash
cargo run -p ojos-orchestrator-tui -- \
  --api-url https://orchestrator.example.com \
  --oidc-issuer https://identity.example.com \
  --oidc-client-id ojos-orchestrator-tui \
  --oidc-audience ojos-orchestrator
```

仅用于前端开发或协议测试的临时 daemon 必须显式声明非持久模式：

```bash
cargo run -p ojos-orchestrator-daemon -- \
  --ephemeral \
  --artifact-root artifacts/orchestrator-dev \
  --bind 127.0.0.1:8090
```

`--ephemeral` 不得用于生产。生产启动前按 [Orchestrator v1.0 运维手册](docs/orchestrator/operations-v1.md) 配置并运行 fail-closed preflight；daemon 不会因 PostgreSQL 不可用而退回内存。

## Store 与 Topology

Store 从可信 Catalog v2 注册、搜索、导入和安装 Release。生产拒绝不可信 Catalog、校验不一致和浮动 OCI tag。Managed 安装明确指定 `target_node_id`，默认启动并在真实健康验证后才显示 Running；External 安装必须验证 endpoint。升级、回滚、卸载和失败补偿都通过持久 Operation/Job 执行。

Topology 编辑 Endpoint/Link 时创建 draft revision。validate/diff 不产生外部副作用；apply/rollback 返回异步 Operation。Rollback 复制旧 Spec 创建新 revision，健康和 drift 只读自 `TopologyStatus`。画布坐标按用户/topology 单独保存，不进入 Spec。

类型化 migration、config/secret、Redis、storage、frontend、Gateway/Auth 和 API registry provider 是 Release pipeline 的内部步骤。计划缺少所需 provider 时直接失败，不返回 Deferred 或假成功。

## 数据与兼容边界

- Desktop 默认使用应用数据目录中的 SQLite、Agent ledger 和 artifact；SQLite 失败时不回退内存。
- 远程生产使用带证书校验 TLS 和连接池的 PostgreSQL，并通过专用 advisory-lock 连接维持单主动所有权。
- 0.2 normalized 数据可 expand-only 导入：旧 topology 成为未应用 draft，旧 runtime 只标记 `External/Unknown`。
- `0.2.0` 兼容构建保留带弃用头的旧路由；`1.0.0` 对旧 mutation 返回 `410 Gone`。旧 Node push/shared-bearer 路径不属于 v1。

## GA 状态

功能和自动化门禁已经进入仓库，真实 PostgreSQL 0.2.0 → 1.0.0 导入以及数据库/artifact
恢复也已纳入 `release.yml` 并通过本地 PG17 验证。GA 仍为 **NO-GO**，直到同一候选 commit 完成：

1. 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operations和完整 24 小时生产 profile 证据；
2. Windows x64 MSI/ZIP、Linux x86_64 DEB/AppImage/tar.gz，以及 SHA256、SPDX SBOM、provenance、Sigstore/attestation 的实际构建和验证。

候选 commit 仍必须重新通过自动化升级/恢复门禁；本地通过结果只证明门禁可执行，不替代候选 CI 结果。

## 文档

- [文档索引](docs/README.md)
- [架构总览](docs/architecture/README.md)
- [产品需求](docs/orchestrator/requirements.md)
- [模块边界](docs/orchestrator/boundary.md)
- [Action 模型](docs/orchestrator/action-model.md)
- [数据库](docs/orchestrator/database.md)
- [Operation/Job 模型](docs/orchestrator/operation-model.md)
- [Topology 模型](docs/orchestrator/topology-model.md)
- [Desktop](docs/orchestrator/desktop.md)
- [Web/TUI 能力一致性](docs/orchestrator/gui-tui-parity.md)
- [v1 运维手册](docs/orchestrator/operations-v1.md)
- [当前状态总结](docs/completeness-summary.md)
- [生产就绪证据](docs/production-readiness.md)
- [发布候选判定](docs/release-candidate.md)

`v0.1.0-alpha` 是 2026-07-03 的历史版本，仍使用旧原生 GUI，不包含当前 v1 Desktop、Store、Topology 或 Agent。
