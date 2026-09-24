# Orchestrator v1.0 架构总览

OJOS Orchestrator 采用控制面与数据面分离的 service-release-first 架构。Catalog/Release 描述可部署内容，Store 负责导入与运行实例，Topology 负责已注册/部署服务之间的期望关系，Operation/Job 负责把异步副作用可靠交给指定 Node。Gateway 承载 OJ 业务流量，不是控制面代理。

## 模块

下图是职责概览，不代表所有旧依赖已经消除。Store/Topology 的宿主用例已分出职责，backend 的纯领域类型和规则直接依赖 `orchestrator-core`；仓储接口归入 storage，正式 Store/诊断使用仓储上下文，历史格式仍通过显式适配复用。当前改造与保留的限制见[重构计划](refactoring-plan.md)。

```text
Desktop / Web / TUI
        │  /api/v1 + SSE
        ▼
orchestrator-backend
        │
        ├─ orchestrator-manager       Catalog / Store use cases
        ├─ orchestrator-control-plane Operation / Job / lease / recovery
        ├─ orchestrator-protocol      shared reports / execution contracts
        ├─ orchestrator-storage       Memory / SQLite / PostgreSQL
        └─ orchestrator-core          pure model / validation / plan / diff
                         │
                         ▼ persistent pull Job
                 orchestrator-agent
                 local SQLite ledger + orchestrator-runtime / Docker Engine
```

- `orchestrator-core` 不访问文件、数据库、网络、进程、环境变量或 Docker；它只定义可测试的领域规则。
- `core/binding_projection` 拥有 Binding generation、激活/撤销和投影增减规则。后台协调与 DurableStore 都调用这一确定性实现；storage 保留 Topology 组提交 DTO 的兼容导出。
- `orchestrator-protocol` 定义共享报告、实例观测、profile、任务载荷和确定性校验，不依赖 Docker 客户端、Agent、数据库或网络传输。backend/storage 直接使用这些契约；Agent 使用 runtime 调用 Docker。旧 runtime 类型路径保留兼容导出，字段与序列化保持一致。详见[执行契约与 Docker 驱动](execution-contracts.md)。
- `orchestrator-storage` 拥有仓储接口及 Memory/Shared/SQLite/PostgreSQL 实现，不依赖 legacy 或 runtime。持久状态以数据库为真值，不维护写后全表重载的内存镜像；节点图与日志声明的纯规则属于 core。详见[仓储边界与历史兼容](repository-boundary.md)。
- `orchestrator-control-plane` 协调至少一次投递、lease、重试、恢复和 saga 补偿；不能证明副作用结果时进入 `NEEDS_ATTENTION`。
- backend 的后台入口只启动和停止循环；`topology_worker/` 内按租约、恢复、网络探测、运行时投影、状态协调和 Job 应用分工，保留原有 CAS/fencing。具体边界见[后台协调](background-coordination.md)。
- `orchestrator-runtime` 只提供固定 Docker Engine/受控运行时操作，不拼接 shell。
- `orchestrator-manager` 的 `catalog_query` 已通过只读端口组合 Catalog 分页和已部署视图；`store/config`、`store/composition` 承载纯配置与组合规则，`store/validation` 通过只读端口编排 Release 校验。这些模块不依赖 HTTP、Console 或数据库实现。整个 crate 仍包含旧 Store Console 应用逻辑，安装和替换用例仍在 backend，继续按计划迁移。
- backend 的 Store HTTP 入口只承担请求与响应映射；宿主用例在 `backend/src/store`，统一通过 `StoreAdmission` 持锁提交任务并撤销失败的拓扑占用。Deployment 生命周期在独立的 `backend/src/deployment.rs`；它和 Release 元数据删除保持不同语义。详情见 [Store 与 Deployment](store.md)。
- `orchestrator-agent` 只执行分配给本 Node 的 Job，并用本地 ledger 决定幂等重放；它还根据 Deployment assignment 原子物化只读 ServiceContext 和短期 workload credential。
- `orchestrator-legacy` 当前容纳 0.2 Console、历史格式适配及领域/仓储类型重导出；它使用 storage 的仓储接口。正式 v1 不再调用旧动作调度，仍通过 `registry_compat` 使用文件加载、投影和诊断格式；显式旧路由与嵌入宿主兼容由 `legacy_console` 单独承接。
- Web 的 API、状态与表单按功能归属，传输实现位于 `shared/api`。实例请求状态和纯展示投影分开；正式调用方不再引用根目录兼容入口。详见 [Web 客户端](web-client.md)。

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

OIDC 控制面身份与 OJ 业务权限之间的查询适配、管理凭据的只读用途，以及保留的身份映射约束见[控制面与 Auth 权限边界](control-plane-auth.md)。

## 跨节点业务数据面

A/B 跨机部署不让 Worker 直连远端数据库或中间件：A 机控制面根据签名 Release v2 和已应用 Topology 生成 ApiBinding；B 机 Agent 通过 mTLS 领取部署任务并物化 context；Worker 使用 Deployment JWT 经 A 的 HTTPS Gateway 按 requirement 名调用 provider。Gateway 从 JWT 推导 consumer 身份并实时检查 Binding、revision 与 credential generation，不信任客户端提交的 caller header。

Problem→Judge 使用 transactional outbox、Redis Stream relay、Judge inbox 和幂等 projection；任务中的题包与源码使用带 binding、相对路径、SHA-256 和 size 的 `ApiResourceRef`。完整规则见 [Service Contract v2](../orchestrator/service-contract-v2.md)。

## 运行形态与边界

- Desktop 的 SQLite 和 artifact 位于 OS 应用数据目录；打开失败时不回退 Memory。Agent ledger 只存在于独立 Node 的私有持久根。
- 远程生产是单主动 PostgreSQL 控制面；专用 advisory-lock 连接保证只有一个 writer。
- Node 使用一次性注册码换取带 SPIFFE Node ID 的 mTLS 证书，并长轮询领取本节点任务；0.2 push/shared bearer 不属于 v1。
- 外部副作用是类型化 pipeline 步骤。API surface 与 ApiBinding 由控制面事务持久化，不依赖外部 API Registry；计划所需 provider、健康实例或 Binding 缺失时 fail fast，不生成 Deferred 或假成功。
- v1 不提供 active-active、通用调度器、自动扩缩容、Kubernetes runtime、任意 shell 或多租户计费。

## 阅读与修改路径

| 变更类型 | 首先阅读 | 边界 |
| --- | --- | --- |
| HTTP/API 行为 | `services/orchestrator/backend/src/*_api.rs` | Store mutation 与 Deployment 生命周期已抽离应用编排；其他路由仍需按重构计划逐项收敛，不继续扩大混合边界。 |
| Store 安装、替换与提交 | `services/orchestrator/backend/src/store` | 命令不接收 HTTP 请求；规划、冲突检查、占用与发布保持既有顺序。锁与补偿由 admission 集中负责。 |
| Catalog 只读查询 | `services/orchestrator/manager/src/catalog_query.rs` | 用例只通过读端口获取数据；`backend/src/adapters/catalog.rs` 对接可信源注册表和仓储投影。查询失败顺序、分页和响应字段保持不变，注册与信任修改不属于读端口。 |
| Release 规则与只读校验 | `services/orchestrator/manager/src/store` | 配置与组合规则只处理显式输入；校验用例没有发布或执行能力；`backend/src/adapters/store_validation.rs` 提供事实读取和运行时规划。 |
| 领域约束与计划 | `services/orchestrator/core/src` | 不引入数据库、网络或运行时依赖。 |
| 持久化与恢复 | `services/orchestrator/storage/src`、`control-plane/src` | 状态以持久记录为准；明确事务和幂等边界。 |
| 服务契约与 SDK | `tools/ojos-service/src/codegen` | `mod.rs` 编排生成、校验与落盘；各语言模块只负责生成产品 SDK。 |
| GoZero 服务启动 | `services/<id>/internal/app`、`internal/svc` | `app` 组装 HTTP 与依赖；`startup.go` 构造并回滚，`servicecontext.go` 持有资源并关闭，`health.go` 保留各自探针。详见[Go 服务生命周期](service-lifecycle.md)。 |
| UI 展示与交互 | `manager/web/src/features`、`views` | 视图组装功能表单；API client 不读取 Store；`control-plane/projection` 只转换显式事实；服务端是权限与状态真值。 |
| 新服务启动 | `services/contest-service/internal/app`、`platform/shared/go/bootstrap` | 进程入口负责配置与信号，应用组装接收显式输入，共享 bootstrap 负责组件生命周期；既有 GoZero 服务仍逐服务保留原适配。 |

Auth 必须连接 PostgreSQL，不保留内存冒烟认证或临时授权投影。构造失败会关闭已创建的连接池和追踪资源；管理员初始化密钥在构造退出时清零。Desktop 不内嵌浏览器测试脚本，控制面不提供测试造数接口。

软件测试、夹具和演练与产品源码物理分离。验证在仓库外的临时源码副本中运行，不能把测试模块复制回工作树。OJ 题目测试数据、判题、健康探针和签名校验属于产品职责，仍在本仓库。

更细的取舍见 [耦合决策](coupling-decisions.md)，持久化见 [编排器数据库](../orchestrator/database.md)，交付方式见 [构建与交付](../release/README.md)。

对象存储以 S3 协议为边界，Storage 的运行适配器与供应端账号/桶初始化分别归属
`internal/store` 和 `internal/objectstoredeploy`。新自托管使用 SeaweedFS，旧 MinIO
配置和数据保留兼容；部署配方与安全切换见[自托管 S3](../../deploy/object-store/README.md)。

目录中两个 `manager` 含义不同：顶层 `manager/` 放客户端与原生安装器，`services/orchestrator/manager` 是 Rust Catalog/Store 应用与旧 Console 适配。本轮保留已被构建、安装器和发布路径引用的目录名；通过模块入口和明确依赖区分职责，不为统一字面命名扩大部署变更。前端已把实现迁到所属功能目录，旧文件名只作为兼容导出保留。
