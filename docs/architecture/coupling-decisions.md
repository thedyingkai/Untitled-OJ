# Orchestrator v1.0 耦合决策

本文记录当前 v1 模块边界。0.2 时代把仓储、Console、下载、进程和 provider 放在同一执行链中的设计已经迁入 `orchestrator-legacy`，不再代表正式架构。

## 1. Core 只保留纯领域规则

`services/orchestrator/core` 可以定义：

- published action 与 RBAC 元数据；
- Catalog/Release v2、Deployment、ApiBinding、Topology、Operation 等领域类型；
- schema/引用/状态迁移校验；
- 确定性 dependency plan、Topology diff 和补偿描述；
- 与具体存储无关的错误和结果契约。

Core 不得读取文件或环境变量，不得连接数据库/HTTP/Docker，不得启动进程，也不得持有进程级可变单例。`core/tests/pure_boundary.rs` 对 crate 源码和依赖方向做边界检查。

这一边界使 Memory、SQLite、PostgreSQL 和 Web/TUI fixture 可以复用相同状态机，而不会把某个适配器行为误当成产品语义。

## 2. 持久状态归 Storage，工作协调归 Control Plane

`orchestrator-storage` 实现三种仓储：

- Memory：测试和显式 `--ephemeral`；
- SQLite：Desktop 持久化，使用 WAL/外键/busy timeout 和旁文件锁；
- PostgreSQL：生产 TLS pool、schema checksum/readiness、session advisory lock。

存储事务只覆盖状态转换、CAS、投影和审计。所有慢 I/O 必须在事务外，因此 v1 不需要全局 console mutex，也不通过写后全表回读建立第二份真值。

`orchestrator-control-plane` 拥有 Operation/Job/attempt/event/lease 协调。Job 至少投递一次；Node 或 provider 的副作用使用稳定 idempotency key。lease 过期后，只有能证明“尚未执行”或副作用本身可幂等重放时才重试，否则进入 `NEEDS_ATTENTION`。

## 3. 外部副作用必须类型化

外部动作放在 `orchestrator-runtime`、`orchestrator-agent` 或 manager provider trait 中：

| 类型 | 正式实现边界 |
| --- | --- |
| 容器生命周期 | Agent 直连 Docker Engine Unix socket/Windows named pipe，按固定请求类型 pull/create/start/stop/restart/remove，并核对实际 RepoDigest。 |
| Gateway/Auth | 受控 HTTP 管理接口；Topology/Store plan 先验证 provider 可用性。 |
| migration | 签名 OCI 一次性任务 + checksum ledger。 |
| config/secret | Agent 根据类型化输入安全物化，不能以任意 shell 替代。 |
| Redis | 已注册连接 ID + 确定性 namespace。 |
| storage | 节点文件目录或 S3-compatible bucket/prefix。 |
| frontend | 原子发布到已配置 Gateway asset store。 |
| API surface / Binding | 签名 Release 的 API 声明和已应用 ApiBinding 直接进入控制面事务仓储；不存在远程 Agent 持有管理凭据的外部 API Registry provider。 |

正式 v1 的规则是 **缺少必需 provider 时 plan 失败**。`planned/deferred/skipped` 不能用作外部执行成功，也不能提升 Deployment、Topology applied head 或 RuntimeInstance observed state。通用 HTTP fallback 只有显式策略启用时可用，且仍受类型化输入和结果检查。

0.2 `Deferred*`、本地进程与 Docker Compose 实现可以留在 `orchestrator-legacy` 供兼容迁移使用，但不得进入 v1 capabilities 或 GA 生产路径。

## 4. Store 与 Topology 不共享写所有权

- Store 决定 Release、Deployment、RuntimeInstance、目标 Node、container 和 OCI digest。
- Topology Spec 只引用已注册或部署的服务，描述 root、Endpoint、Link 和命名 `api_bindings` 的期望关系。
- 持久 ApiBinding 由已应用 revision 派生，唯一键是 `(consumer_deployment_id, requirement_name)`；Store 安装只能根据用户确认的映射创建 draft/revision，不能绕过 Topology apply 直接激活路由。
- Topology Status 记录 provider/runtime 观测、drift 和最后 Operation。
- Operation/Job/audit 是独立执行记录，不能塞入 Spec。
- UI layout 按 user/topology 存储，不能影响业务 revision digest。

因此 Topology apply 不会隐式安装服务；Store install 也不会直接改写 applied topology。需要连接变化时，通过 draft revision 与正常 apply 明确表达。

## 5. Agent 采用 pull，并隔离 Node 与 workload 身份

Node 用一次性注册码换取带 SPIFFE Node ID 的 mTLS 证书。Agent 只 claim 分配给自身的持久 Job，并以本地 SQLite ledger 记录 attempt、副作用结果和 replay 决策。证书可续签和即时吊销。

这条边界删除了共享 `ORCHESTRATOR_INTERNAL_TOKEN` 的 Node push 语义：Node 不持有可反向调用人类控制 API 的共享控制面 token，控制面也不向 Node 发送 shell 字符串。远程 Agent 不保存 Auth/Gateway/API Registry 管理凭据。

Agent 只凭 Node mTLS 与 Deployment assignment 兑换 15 分钟 workload JWT，并在到期前原子替换 Deployment 私有 token 文件。容器只读挂载 `/run/ojos/service`；context 含 Gateway origin、CA、命名 Binding 和 generation，不含 Node 私钥或管理 token。Desktop 的 loopback Agent 复用同一 Job/runtime/context 语义，只是传输限定在本机。

## 6. 所有客户端只耦合 `/api/v1`

| 入口 | 对控制面的关系 |
| --- | --- |
| Desktop WebView | 同源 `/api/v1`；一次性 bootstrap secret 兑换 HttpOnly session。 |
| 远程 Web | 同源 `/api/v1`；OIDC Code + PKCE。 |
| TUI | 远程 `/api/v1`；OIDC Device Flow，token 仅存内存。 |
| 自动化 | HTTPS REST + SSE；使用服务端 capabilities 和 RBAC。 |

TUI 不再直接链接 Console 完成 mutation。Web 和 TUI 的差别只在交互表达；published action、默认值、Problem、cursor、ETag、SSE 和 Idempotency-Key 语义必须一致。

`platform/schemas/orchestrator/openapi-v1.yaml` 与 `actions-v1.yaml` 是唯一正式契约。旧 forms/action catalog、无版本路由和通用 CRUD 只属于 0.2 兼容层。

## 7. 兼容代码不能反向污染 v1

允许的依赖方向是 v1 组合根显式调用必要的兼容导入适配；`orchestrator-core` 不能依赖 legacy，正式 API/clients 也不能从 legacy action 推断 capabilities。

旧 normalized PostgreSQL 数据只做一次 expand-only 导入：Topology snapshot 成为未应用 draft，HostService 成为 `External/Unknown` 投影。导入不伪造运行时事实，也不破坏旧表。

## 8. 已接受的 v1 约束

单主动控制面、显式节点放置、Docker Engine 和最多 100 Nodes 是 v1 产品边界，不是待拆耦问题。active-active、自动 failover、自动扩缩容、Kubernetes、多租户和任意命令执行均不在本版本范围。

当前实现与证据边界只以[项目状态总结](../completeness-summary.md)为准；额外容量和签名要求见[可选上线证据](../unfinished/README.md)。
