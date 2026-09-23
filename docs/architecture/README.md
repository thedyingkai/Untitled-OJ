# Orchestrator v1.0 架构总览

OJOS Orchestrator 采用控制面与数据面分离的 service-release-first 架构。Catalog/Release 描述可部署内容，Store 负责导入与运行实例，Topology 负责已注册/部署服务之间的期望关系，Operation/Job 负责把异步副作用可靠交给指定 Node。Gateway 承载 OJ 业务流量，不是控制面代理。

## 模块

下图是职责概览，不代表这些边界已经完全由依赖关系保证。当前后端仍有较大的 Store/Topology 用例实现，`orchestrator-manager` 仍包含旧 Console 应用逻辑，正式链路仍通过 `orchestrator-legacy` 使用部分领域类型和基础能力。正在按[重构计划](refactoring-plan.md)逐项收敛，不能把规划当作现状。

```text
Desktop / Web / TUI
        │  /api/v1 + SSE
        ▼
orchestrator-backend
        │
        ├─ orchestrator-manager       Catalog / Store use cases
        ├─ orchestrator-control-plane Operation / Job / lease / recovery
        ├─ orchestrator-protocol      shared runtime reports / closed profiles
        ├─ orchestrator-storage       Memory / SQLite / PostgreSQL
        ├─ orchestrator-runtime       Docker Engine + typed runtime contracts
        └─ orchestrator-core          pure model / validation / plan / diff
                         │
                         ▼ persistent pull Job
                 orchestrator-agent
                 local SQLite ledger + Docker Engine
```

- `orchestrator-core` 不访问文件、数据库、网络、进程、环境变量或 Docker；它只定义可测试的领域规则。
- `orchestrator-protocol` 定义控制面与 Agent 共享的运行时报告、实例观测、profile 和 health policy，仅依赖 serde/错误类型派生。daemon 不再依赖 Agent crate；runtime 和 Agent 暂时保留原类型路径的兼容导出。其余执行 payload 仍在 runtime，不能据此声称整个执行层已解耦。
- `orchestrator-storage` 是持久状态真值，不维护写后全表重载的内存镜像。
- `orchestrator-control-plane` 协调至少一次投递、lease、重试、恢复和 saga 补偿；不能证明副作用结果时进入 `NEEDS_ATTENTION`。
- `orchestrator-runtime` 只提供固定 Docker Engine/受控运行时操作，不拼接 shell。
- `orchestrator-manager` 当前同时包含 Catalog/Release v2 模型与旧 Store Console 应用逻辑；正式 v1 安装、校验和替换用例仍主要位于 backend，尚未完成应用层归属调整。
- `orchestrator-agent` 只执行分配给本 Node 的 Job，并用本地 ledger 决定幂等重放；它还根据 Deployment assignment 原子物化只读 ServiceContext 和短期 workload credential。
- `orchestrator-legacy` 当前容纳 0.2 Console、仓储接口、适配器及领域类型重导出。正式 v1 仍有依赖；目标是迁出正式能力，让它只承担兼容转换，尚不能声称已隔离。

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

## 阅读与修改路径

| 变更类型 | 首先阅读 | 边界 |
| --- | --- | --- |
| HTTP/API 行为 | `services/orchestrator/backend/src/*_api.rs` | 当前 Store API 仍混有用例规则；新增代码应按重构计划分离接入与用例，不继续扩大该混合边界。 |
| 领域约束与计划 | `services/orchestrator/core/src` | 不引入数据库、网络或运行时依赖。 |
| 持久化与恢复 | `services/orchestrator/storage/src`、`control-plane/src` | 状态以持久记录为准；明确事务和幂等边界。 |
| 服务契约与 SDK | `tools/ojos-service/src/codegen` | `mod.rs` 编排生成、校验与落盘；各语言模块只负责生成产品 SDK。 |
| Auth 启动 | `services/auth-service/internal/svc` | `servicecontext.go` 组装与生命周期，`environment.go` 环境配置，`workload_identity.go` 身份与授权。 |
| UI 展示 | `manager/web/src` | API client、store 与视图分工；服务端是权限与状态真值。 |

Auth 必须连接 PostgreSQL，不保留内存冒烟认证或临时授权投影。构造失败会关闭已创建的连接池和追踪资源；管理员初始化密钥在构造退出时清零。Desktop 不内嵌浏览器测试脚本，控制面不提供测试造数接口。

软件测试、夹具和演练与产品源码物理分离。验证在仓库外的临时源码副本中运行，不能把测试模块复制回工作树。OJ 题目测试数据、判题、健康探针和签名校验属于产品职责，仍在本仓库。

更细的取舍见 [耦合决策](coupling-decisions.md)，持久化见 [编排器数据库](../orchestrator/database.md)，交付方式见 [构建与交付](../release/README.md)。

目录中两个 `manager` 含义不同：顶层 `manager/` 放客户端与原生安装器，`services/orchestrator/manager` 是 Rust Catalog/旧 Store 代码。当前不做全仓路径改名，先完成职责拆分，再在计划最后阶段处理命名。
