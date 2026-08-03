# Orchestrator v1.0 产品需求

OJOS Orchestrator 是服务控制面。它管理服务发布、节点放置、运行实例、期望连接关系、持久操作和诊断，不承载题目、提交、判题、用户、比赛或站点等 OJ 业务。

## 正式交付形态

- **Desktop**：Tauri 原生 WebView 内嵌同源 Web UI、随机 loopback backend 和 loopback Agent；默认使用操作系统应用数据目录中的 SQLite，不打开外部浏览器。
- **远程控制面**：单主动 daemon + PostgreSQL，目标规模最多 100 Nodes；生产模式强制 PostgreSQL TLS、HTTPS、OIDC、Node CA、可信 Catalog 和 durable artifact 目录。
- **Node**：独立 Agent 通过带 SPIFFE Node ID 的 mTLS pull 协议领取持久 Job，并通过 Docker Engine API 执行分配给本节点的固定任务。
- **TUI**：远程 `/api/v1` 客户端，使用 OIDC Device Authorization Grant；不在进程内执行控制面 mutation。

Memory store 只允许显式 `--ephemeral` 的开发/测试。Desktop SQLite 或生产 PostgreSQL 打开失败时均不得回退内存。

## 领域对象与所有权

### Store

Store 拥有：

- `CatalogSource`、Catalog v2 package/release metadata；
- 已导入的 `ServiceRelease`；
- `Deployment` 与 `RuntimeInstance`；
- 节点放置、container ID、实际 RepoDigest、desired/observed state 和 health。

Catalog v2 必须表达 semver、channel、平台、最低编排器版本、依赖、metadata SHA-256、OCI digest 和签名。生产只接受可信 Catalog 和不可变 OCI digest。`runtime_capabilities` 属于具体 release version，并包含在 RFC 8785/JCS + Ed25519 签名负载中；字段缺失等同于空集，不能从同一服务的其他版本继承。`link-probe-v1` 还要求同一 metadata manifest 的 `apis` 精确声明 `orchestrator.link-probe.v1`（HTTP、default port、`GET /probe`、global/public/stable/v1、无 caller filter），两侧不一致时以 `CATALOG_METADATA_CAPABILITY_MISMATCH` 在导入或计划产生任何副作用前拒绝。

### Topology

Topology 只拥有已经注册或部署服务之间的期望关系：

- `TopologySpec`：topology id、root/authority、Endpoint 与 Link 的期望字段；
- `TopologyRevision`：不可变 revision、父 revision、内容 digest、创建者和说明；
- `TopologyStatus`：desired/observed revision、Deployment/Endpoint/Link 实际健康、drift 和最后 Operation。

Operation、日志、诊断、容器 ID、实时健康和画布坐标不得写入 Spec。Rollback 复制旧 Spec 生成新 revision，再执行正常 apply。

### 执行与审计

- `Operation` 聚合 plan、confirm、apply、cancel、retry、rollback 及日志/事件。
- `Job` 使用 `QUEUED → LEASED → SUCCEEDED/RETRY_WAIT/FAILED/CANCELLED/NEEDS_ATTENTION` 状态机。
- Node 本地 SQLite ledger 保存 attempt 与副作用结果；控制面保存 Job、attempt、event、lease 和 Operation 投影。
- 所有 mutation 的审计 intent 必须先持久化；审计不可写时不得开始外部副作用。

## Store 执行语义

- “仅导入”只注册并校验 Release，不调用 Docker。
- Managed 安装明确指定 `target_node_id`，默认 `start=true`；流程为 plan/confirm、artifact 交付、pull/create/start、健康验证和投影提升。
- Managed `endpoint` 是 `ip:host_port:service_id` 的类型化绑定：IP 必须等于目标 Node 的 `host_ip`，service 必须等于选中的 release；Docker 只把签名 manifest 的 backend container port 映射到该 host port。Runtime 投影必须保留 endpoint、精确 `release_version`、container ID 和实际 RepoDigest。升级/回滚复用仍被旧实例占用的 host port 时 fail closed，必须显式提供新的 port 才能执行无重叠切换。
- External 安装不接管进程，但必须验证 endpoint 真实健康。
- 安装失败保留 imported Release，补偿 Deployment、container、Endpoint、route/provider 副作用；不能显示 installed/running。
- 升级在旧实例仍可恢复时创建并验证新实例，健康且切换完成后才移除旧实例；失败恢复旧实例。
- migration、config/secret、Redis、storage、frontend、Gateway/Auth 和 API registry 都是 Release pipeline 的类型化步骤。缺少所需 provider 时必须在 plan 阶段拒绝，不能返回 Deferred 或假成功。

## Topology 执行语义

- Endpoint/Link 编辑只创建新的 draft revision；apply 才创建异步 Operation。
- validate 检查 schema、root、重复 ID、悬空引用、服务/Deployment 引用和 provider 前置条件。
- diff 对规范化 Spec 产生字节级稳定结果。
- apply 使用 saga 与类型化补偿；所有必要步骤成功后才推进 applied head。
- 结果不可证明时 Job/Operation 进入 `NEEDS_ATTENTION`；补偿失败时 Status 为 `DEGRADED`，reconciler 持续对账。
- Web/TUI 只能读取正式 `TopologyStatus` 展示健康和 drift。
- 每条启用 Link 的 source Endpoint 必须恰好绑定一个已验证的 RuntimeInstance，并按该实例的精确 `release_version` 找到同时通过 Catalog/manifest 双重声明的 link-probe 能力；缺失、多重绑定或版本不符分别以 `TOPOLOGY_LINK_PROBE_RELEASE_BINDING_REQUIRED` / `TOPOLOGY_LINK_PROBE_CAPABILITY_REQUIRED` 拒绝 validate/apply/rollback。状态对 Endpoint 执行真实 `/health` 请求，对 Link 执行 source `/probe?target=...` 请求；不得使用隐藏路由、provider 自报或同服务其他版本的能力代替网络证据。历史 revision diff 只比较不可变 Spec，不依赖当前 Runtime/Catalog 状态。

## API 与 action

唯一正式前缀为 `/api/v1`。OpenAPI 和 `actions-v1.yaml` 是 router、dispatcher、RBAC、Web 与 TUI 的共同契约：

- catalog/release：来源管理、导入、校验、安装、升级、回滚、删除；
- node：注册码、续签/吊销、列表、健康、drain、移除；
- deployment：查询、启动、停止、重启、卸载、健康；
- topology：draft、revision、validate、diff、apply、rollback、status、export；
- operation：plan、confirm、apply、cancel、retry、rollback、logs/events；
- diagnostic：create、list、get、export。

Endpoint/Link mutation 只存在于 topology draft。route、frontend、migration、permission、redis、storage、config 和 secret 不发布通用 CRUD action。

协议要求：成功响应包含 `request_id`，失败使用 `application/problem+json`；plan/资源创建返回 `201`，长操作返回 `202 + operation_id`；mutation 强制 `Idempotency-Key`，集合使用 cursor，Revision 使用 ETag/`If-Match`，Operation 事件使用可从 `Last-Event-ID` 恢复的 SSE。published action 矩阵必须为零 `UNSUPPORTED`。

## 身份和入口一致性

- Desktop 用一次性 bootstrap secret 兑换 HttpOnly 本地 admin 会话，不把 token 写入 `localStorage`。
- 远程 Web 使用 OIDC Authorization Code + PKCE；TUI 使用 OIDC Device Flow。
- RBAC 固定为 viewer/operator/admin。
- Web 与 TUI 覆盖同一 published action、默认值、状态、Problem、cursor、ETag、SSE 和幂等语义；交互布局可以不同。

## 兼容与非目标

`0.2.0` 兼容构建可保留带弃用头的无版本旧路由。`1.0.0` 对旧 mutation 返回 `410 Gone`，不提供旧 Node push/shared-bearer 路径。

v1 不实现 active-active HA、自动 failover、自动扩缩容、通用调度器、Kubernetes runtime、多租户计费或任意本地命令执行。服务节点由安装请求显式选择；Topology 不会隐式安装服务。
