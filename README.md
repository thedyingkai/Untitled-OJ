# OJOS Orchestrator

OJOS Orchestrator 是 OJOS 的服务控制面。它负责可信 Catalog/Release、显式节点放置、Deployment 生命周期、版本化 Topology、持久 Operation/Job、观测和诊断；题目、提交、判题、用户、比赛等业务仍由各 Service 实现。

当前源码版本为 `1.0.0`。本地功能与 portable 交付按仓库自动化判定；生产规模证明和代码签名只在需要对应容量声明或公开受信任分发时执行，不再阻塞本地功能完成。

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
- `ApiBinding` / `ServiceContext` / `RuntimeReport`：把 Release v2 的命名 API 依赖解析到已应用 Topology，并把短期 workload 身份和真实节点能力安全物化给 Deployment。
- `Operation` / `Job`：计划、确认、异步执行、lease、重试、补偿、日志和恢复。
- `Node`：使用 SPIFFE Node ID mTLS 证书领取持久 Job 的独立 Agent。
- `Diagnostic`：面向 Deployment、Topology 和 Operation 的只读诊断结果。

Store 负责安装和节点放置；Topology 只连接已经注册或部署的服务，不会隐式安装服务。跨节点业务调用统一遵循 [Service Contract v2](docs/orchestrator/service-contract-v2.md)：consumer 只按 requirement 名访问 Gateway，不能拼接远端服务地址或持有 A 机数据库、中间件和管理凭据。

## 正式入口

- **Desktop**（`manager/desktop`）：默认入口。在 Tauri 原生 WebView 中加载同源 Web UI，并启动随机 loopback backend 与 loopback Agent；默认使用 OS 应用数据目录中的 SQLite，不打开外部浏览器。
- **远程 Web**（`manager/web`）：由生产 daemon 托管，使用 OIDC Authorization Code + PKCE 和 HttpOnly 会话。
- **TUI**（`manager/tui`）：远程 `/api/v1` 客户端，使用 OIDC Device Authorization Grant，不在进程内执行 mutation。
- **daemon**（`services/orchestrator/backend`）：单一 REST/SSE 控制面。生产必须使用 PostgreSQL、TLS、OIDC、Node CA、可信 Catalog 和 durable artifact 目录。

Web 与 TUI 使用相同的 published action、RBAC、默认值、Problem Details、cursor、ETag、SSE 和 Idempotency-Key 语义。TUI 不复刻拖拽画布，但控制能力一致。

## 一条命令安装

不需要 MSI、脚本安装器、管理员权限或云签名。下载并解压对应平台的 unsigned portable 包后，直接运行包内的原生 `ojos-orchestrator install`。它会验证目标平台、完整资源布局和每个文件的 SHA-256，再以带锁、可恢复的目录切换安装到当前用户目录；升级不会删除 Desktop SQLite、Agent ledger 或用户数据。

```powershell
# Windows；在解压后的包目录运行，默认安装到 %LOCALAPPDATA%\Programs\OJOS-Orchestrator
.\ojos-orchestrator.exe install
# 当前终端可直接启动；新终端可使用 ojos-orchestrator
& "$env:LOCALAPPDATA\Programs\OJOS-Orchestrator\bin\ojos-orchestrator.exe"
```

```bash
# Linux；在解压后的包目录运行，默认安装到 ~/.local/share/ojos-orchestrator
./ojos-orchestrator install
~/.local/share/ojos-orchestrator/bin/ojos-orchestrator
```

Windows 原生安装器会更新当前用户 PATH，但当前终端不会因此改变；新开终端后才能直接输入 `ojos-orchestrator`。升级前须退出正在运行的 Desktop、daemon、TUI 和 Agent；各程序持有共享运行锁，安装器不会让旧二进制混用新资源。Linux 不修改任何 shell 配置，会输出完整启动路径；需要短命令时把安装目录的 `bin` 加入自己的 PATH。Linux 包以 Ubuntu 24.04 x86_64 为当前构建基线，Desktop 运行仍要求系统已安装 WebKitGTK 4.1、GTK3、Ayatana AppIndicator、librsvg 和 libxdo；安装器会用 `ldd` 明确拒绝缺库环境。安装器本身不调用 shell、批处理或 PowerShell 脚本。

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

类型化 migration、config/secret、Redis、storage、frontend 与 Gateway/Auth 投影是 Release pipeline 的内部步骤。API surface 与 `ApiBinding` 由控制面事务持久化，不依赖外部 API Registry provider。计划缺少所需 provider 或 Binding 时直接失败，不返回 Deferred 或假成功。

## 数据与兼容边界

- Desktop 默认使用应用数据目录中的 SQLite、Agent ledger 和 artifact；SQLite 失败时不回退内存。
- 远程生产使用带证书校验 TLS 和连接池的 PostgreSQL，并通过专用 advisory-lock 连接维持单主动所有权。
- 0.2 normalized 数据可 expand-only 导入：旧 topology 成为未应用 draft，旧 runtime 只标记 `External/Unknown`。
- `0.2.0` 兼容构建保留带弃用头的旧路由；`1.0.0` 对旧 mutation 返回 `410 Gone`。旧 Node push/shared-bearer 路径不属于 v1。

## 交付状态

功能状态只以[项目状态总结](docs/completeness-summary.md)为准。该页区分当前工作树实现、本地 pre-commit 双 Engine 证据、最终 commit 证据，以及尚未执行的容量、签名 GA 和安全验收；本 README 不单独作 GO 判定。

普通交付使用 unsigned portable ZIP/tar 和 `ojos-orchestrator install`，不依赖 MSI、Azure、专用 runner 或主机群。

仓库仍保留 100 Node/24 小时容量工具和 signed-GA workflow，供未来需要声明该生产规模或面向受签名策略约束的公开分发时使用。它们是可选的额外证据，不代表 Store、Topology、Desktop 或 CLI 功能未完成。

## 文档

- [文档索引](docs/README.md)
- [架构总览](docs/architecture/README.md)
- [产品需求](docs/orchestrator/requirements.md)
- [模块边界](docs/orchestrator/boundary.md)
- [Action 模型](docs/orchestrator/action-model.md)
- [数据库](docs/orchestrator/database.md)
- [Operation/Job 模型](docs/orchestrator/operation-model.md)
- [Topology 模型](docs/orchestrator/topology-model.md)
- [Service Contract v2](docs/orchestrator/service-contract-v2.md)
- [工作负载凭据边界](docs/orchestrator/credential-boundary-v2.md)
- [A/B 跨机完整门禁](deploy/cross-machine/README.md)
- [Judge Worker 生产部署](deploy/worker/README.md)
- [Service SDK](sdk/service-sdk/README.md)
- [Desktop](docs/orchestrator/desktop.md)
- [Web/TUI 能力一致性](docs/orchestrator/gui-tui-parity.md)
- [v1 运维手册](docs/orchestrator/operations-v1.md)
- [当前状态总结](docs/completeness-summary.md)
- [生产就绪证据](docs/production-readiness.md)
- [发布候选判定](docs/release-candidate.md)

`v0.1.0-alpha` 是 2026-07-03 的历史版本，仍使用旧原生 GUI，不包含当前 v1 Desktop、Store、Topology 或 Agent。
