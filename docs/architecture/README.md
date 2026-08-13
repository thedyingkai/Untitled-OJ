# Orchestrator v1.0 架构总览

OJOS Orchestrator 采用控制面与数据面分离的 service-release-first 架构。Catalog/Release 描述可部署内容，Store 负责导入与运行实例，Topology 负责已注册/部署服务之间的期望关系，Operation/Job 负责把异步副作用可靠交给指定 Node。Gateway 承载 OJ 业务流量，不是控制面代理。

## 模块

```text
Desktop / Web / TUI
        │  /api/v1 + SSE
        ▼
orchestrator-backend
        │
        ├─ orchestrator-manager       Catalog / Store use cases
        ├─ orchestrator-control-plane Operation / Job / lease / recovery
        ├─ orchestrator-storage       Memory / SQLite / PostgreSQL
        ├─ orchestrator-runtime       Docker Engine + typed runtime contracts
        └─ orchestrator-core          pure model / validation / plan / diff
                         │
                         ▼ persistent pull Job
                 orchestrator-agent
                 local SQLite ledger + Docker Engine
```

- `orchestrator-core` 不访问文件、数据库、网络、进程、环境变量或 Docker；它只定义可测试的领域规则。
- `orchestrator-storage` 是持久状态真值，不维护写后全表重载的内存镜像。
- `orchestrator-control-plane` 协调至少一次投递、lease、重试、恢复和 saga 补偿；不能证明副作用结果时进入 `NEEDS_ATTENTION`。
- `orchestrator-runtime` 只提供固定 Docker Engine/受控运行时操作，不拼接 shell。
- `orchestrator-manager` 把 Catalog、Release、Store 和类型化 provider 组合成产品用例。
- `orchestrator-agent` 只执行分配给本 Node 的 Job，并用本地 ledger 决定幂等重放；它还根据 Deployment assignment 原子物化只读 ServiceContext 和短期 workload credential。
- `orchestrator-legacy` 隔离 0.2 Console、旧仓储、旧路由和本地/Compose 适配，不能作为 v1 生产依赖方向的反向入口。

## 状态所有权

| 状态 | 所有者 | 说明 |
| --- | --- | --- |
| Catalog/Release | Store | 版本、平台、依赖、签名、OCI digest 和导入状态。 |
| Deployment/RuntimeInstance | Store/runtime projection | 节点、container、实际 RepoDigest、desired/observed state 和 health。 |
| TopologySpec/Revision | Topology | 已注册/部署服务之间的期望 Endpoint/Link；Revision 不可变。 |
| ApiBinding | Topology/storage | requirement 名、consumer/provider Deployment、API/version、Link/revision、Gateway 路径、generation、desired/observed state 与 drift。 |
| TopologyStatus | Topology reconciler | observed revision、健康、链路、drift 和最后 Operation。 |
| Operation/Job/Event/Audit | control plane/storage | 计划、确认、执行、lease、恢复、日志和 append-only 审计。 |
| 画布坐标 | per-user UI state | 不进入 TopologySpec。 |
| Node 执行结果 | Agent ledger + control-plane Job | ledger 决定本地副作用是否可安全重试；控制面保存全局投影。 |
| RuntimeReport/ServiceContext | Agent + control plane | Agent 上报不可编辑的 Docker/cgroup/policy facts；控制面只持久化 context 引用与 generation，不保存明文 workload token。 |

Store 安装负责服务放置，Topology 不隐式安装服务。Endpoint/Link 编辑先形成 draft；apply 才产生 Operation。Rollback 不修改旧 revision，而是复制旧 Spec 创建新 revision。

## 正式入口

- Desktop：Tauri WebView 内嵌同源 Web UI 与随机 loopback backend，默认 SQLite，不打开外部浏览器；本机 managed execution 为 `Unavailable`，容器任务只能交给独立 Agent。
- 远程 Web：daemon 托管同一份 Vue bundle，使用 OIDC Authorization Code + PKCE 和 HttpOnly 会话。
- TUI：OIDC Device Flow 的 `/api/v1` 客户端，不在进程内调用 core 执行 mutation。
- daemon API：单一 `/api/v1` REST/SSE 契约，生产使用 PostgreSQL、TLS、OIDC 和固定 viewer/operator/admin RBAC。

Web/TUI 只根据 published capabilities 显示操作。HTTP `202` 只表示异步 Operation 已接受，最终成功必须读取持久 Operation/Status。

## 跨节点业务数据面

A/B 跨机部署不让 Worker 直连远端数据库或中间件：A 机控制面根据签名 Release v2 和已应用 Topology 生成 ApiBinding；B 机 Agent 通过 mTLS 领取部署任务并物化 context；Worker 使用 Deployment JWT 经 A 的 HTTPS Gateway 按 requirement 名调用 provider。Gateway 从 JWT 推导 consumer 身份并实时检查 Binding、revision 与 credential generation，不信任客户端提交的 caller header。

Problem→Judge 使用 transactional outbox、Redis Stream relay、Judge inbox 和幂等 projection；任务中的题包与源码使用带 binding、相对路径、SHA-256 和 size 的 `ApiResourceRef`。完整规则见 [Service Contract v2](../orchestrator/service-contract-v2.md)。

## 运行形态与边界

- Desktop 的 SQLite 和 artifact 位于 OS 应用数据目录；打开失败时不回退 Memory。Agent ledger 只存在于独立 Node 的私有持久根。
- 远程生产是单主动 PostgreSQL 控制面；专用 advisory-lock 连接保证只有一个 writer。
- Node 使用一次性注册码换取带 SPIFFE Node ID 的 mTLS 证书，并长轮询领取本节点任务；0.2 push/shared bearer 不属于 v1。
- 外部副作用是类型化 pipeline 步骤。API surface 与 ApiBinding 由控制面事务持久化，不依赖外部 API Registry；计划所需 provider、健康实例或 Binding 缺失时 fail fast，不生成 Deferred 或假成功。
- v1 不提供 active-active、通用调度器、自动扩缩容、Kubernetes runtime、任意 shell 或多租户计费。

更细的取舍见 [耦合决策](coupling-decisions.md)，持久化见 [编排器数据库](../orchestrator/database.md)，发布状态见 [项目状态总结](../completeness-summary.md)。
