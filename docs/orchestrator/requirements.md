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
- 节点不可编辑的 `RuntimeReport`、runtime policy/profile 引用与实际 HostConfig digest；
- 节点放置、container ID、实际 RepoDigest、desired/observed state 和 health。

Catalog v2 必须表达 semver、channel、平台、最低编排器版本、依赖、metadata SHA-256、OCI digest 和签名。生产只接受可信 Catalog 和不可变 OCI digest。Release v2 的 `provides.apis`、`requires.apis`、events 和 `runtime_contract` 属于具体 release version，并进入 RFC 8785/JCS + Ed25519 签名负载；字段不能从同一服务的其他版本继承。Catalog 与 metadata/manifest 不一致时必须在导入或 plan 产生任何副作用前拒绝。

### Topology

Topology 只拥有已经注册或部署服务之间的期望关系：

- `TopologySpec`：topology id、root/authority、Endpoint 与 Link 的期望字段；
- `TopologyRevision`：不可变 revision、父 revision、内容 digest、创建者和说明；
- `TopologyStatus`：desired/observed revision、Deployment/Endpoint/Link 实际健康、drift 和最后 Operation。
- `ApiBinding`：Link 中 requirement 到 provider API 的选择，以及 consumer/provider Deployment、Gateway 路径、timeout、permission、credential generation 和 desired/observed state。

Operation、日志、诊断、容器 ID、实时健康和画布坐标不得写入 Spec。Rollback 复制旧 Spec 生成新 revision，再执行正常 apply。

### 执行与审计

- `Operation` 聚合 plan、confirm、apply、cancel、retry、rollback 及日志/事件。
- `Job` 使用 `QUEUED → LEASED → SUCCEEDED/RETRY_WAIT/FAILED/CANCELLED/NEEDS_ATTENTION` 状态机。
- Node 本地 SQLite ledger 保存 attempt 与副作用结果；控制面保存 Job、attempt、event、lease 和 Operation 投影。
- 所有 mutation 的审计 intent 必须先持久化；审计不可写时不得开始外部副作用。

## Store 执行语义

- “仅导入”只注册并校验 Release，不调用 Docker。
- Managed 安装明确指定 `target_node_id`，默认 `start=true`；流程为 plan/confirm、artifact 交付、pull/create/start、健康验证和投影提升。
- 对需要网络入口的 provider，Managed endpoint 必须来自目标 Node facts 与签名 backend port，并在 Runtime 投影保留精确 `release_version`、container ID 和实际 RepoDigest。`backend-worker` 等无入站业务端口的 consumer 不伪造 host port；它通过 ApiBinding 和 Gateway 出站调用 provider。升级/回滚不能覆盖仍被旧实例占用的 endpoint，必须先健康新实例并原子切换 Binding。
- External 安装不接管进程，但必须验证 endpoint 真实健康。
- 安装失败保留 imported Release，补偿 Deployment、container、Endpoint、route/provider 副作用；不能显示 installed/running。
- 升级在旧实例仍可恢复时创建并验证新实例，健康且切换完成后才移除旧实例；失败恢复旧实例。
- migration、config/secret、Redis、storage、frontend 与 Gateway/Auth 投影都是 Release pipeline 的类型化步骤。API surface 直接来自签名 Release，ApiBinding 由控制面事务持久化；不存在外部 API Registry provider。缺少所需 provider、健康实例或显式 Binding 时必须在 plan 阶段拒绝，不能返回 Deferred 或假成功。

## Service Contract v2

- Release 使用命名 `provides.apis`/`requires.apis`、SemVer 范围、events 与不可变 `runtime_contract`；旧 `apis`、`required_apis` 和 `service_identity.allowed_apis` 只可兼容导入，生产安装前必须完成无歧义转换。
- Resolver 只使用 applied Topology、显式 Link、签名 Release、Running/Healthy RuntimeInstance 和 60 秒内的新鲜 RuntimeReport。零候选拒绝；多个候选必须显式选择；唯一推荐也必须随 Operation 确认。
- Agent 为每个 Deployment 原子物化只读 ServiceContext，并使用 Node mTLS 兑换短期 Deployment JWT。Gateway 从 JWT 推导 deployment/service/node，不信任 caller header；每次请求实时校验活动 Binding、Topology revision 和 generation。
- 业务任务的题包、源码和大文件使用 `ApiResourceRef`，只包含 binding、API ID、相对路径、SHA-256 和 size。Worker 通过 SDK 流式下载并校验，不能接收绝对 URL、共享目录或管理凭据。
- 普通 Service 使用 `standard-container-v1`。只有签名且 digest-pinned 的 Judge Worker 可引用 `judge-sandbox-v1`，并要求节点本地 policy 精确允许 profile digest 与 OCI artifact；安装请求不能覆盖 capability、mount、security option 或 host path。

## Topology 执行语义

- Endpoint/Link 编辑只创建新的 draft revision；apply 才创建异步 Operation。
- validate 检查 schema、root、重复 ID、悬空引用、服务/Deployment 引用、API SemVer、Binding 唯一性、RuntimeReport 新鲜度和 provider 前置条件。
- diff 对规范化 Spec 产生字节级稳定结果。
- apply 使用 saga 与类型化补偿；所有必要步骤成功后才推进 applied head。
- 结果不可证明时 Job/Operation 进入 `NEEDS_ATTENTION`；补偿失败时 Status 为 `DEGRADED`，reconciler 持续对账。
- Web/TUI 只能读取正式 `TopologyStatus` 展示健康和 drift。
- 每个必需 requirement 必须恰好解析到用户确认的健康 provider。apply 先暂存 Gateway/Auth/ApiBinding 投影，consumer 健康后才推进 applied head；失败删除暂存路由与身份，补偿不确定时进入 `NEEDS_ATTENTION`。历史 revision diff 只比较不可变 Spec，不依赖当前 Runtime/Catalog 状态。

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
- workload 身份与人类 RBAC 分离：Auth 为每 Deployment 签发面向 Gateway 的 15 分钟 Ed25519 JWT；断开 Link、卸载或 rebind 通过 generation 立即使旧 token 失效，且不得回退 Auth admin bearer。
- Web 与 TUI 覆盖同一 published action、默认值、状态、Problem、cursor、ETag、SSE 和幂等语义；交互布局可以不同。

## 兼容与非目标

`0.2.0` 兼容构建可保留带弃用头的无版本旧路由。`1.0.0` 对旧 mutation 返回 `410 Gone`，不提供旧 Node push/shared-bearer 路径。

v1 不实现 active-active HA、自动 failover、自动扩缩容、通用调度器、Kubernetes runtime、多租户计费或任意本地命令执行。服务节点由安装请求显式选择；Topology 不会隐式安装服务。
