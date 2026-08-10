# Orchestrator v1.0 当前状态

本文是 Orchestrator 唯一的功能状态源。其他 README、运维文档和发布文档只描述契约、操作方法或额外证据要求；若结论冲突，以本页为准。

## 当前结论

当前工作树已经实现 Service Contract v2 的主要闭环，并在本地 pre-commit 环境通过一次 `full-components` 双 Engine 门禁。这个结果证明当前工作树在单台 Linux 测试环境中的两个隔离 Docker Engine 上能够完成 A 机业务栈、B 机 Agent/Store Judge Worker、Problem 事件投影、Gateway Binding、资源校验下载、nsjail 判题、结果回传、失败补偿、恢复和通用 provider/consumer fixture。

它不是最终发布结论：工作树尚未冻结为同一 clean commit，门禁也不是两台物理主机。当前没有 100 Node/24 小时容量证据、签名 GA 制品或最终安全验收，因此不得据此声称已经验证这些事项，也不得创建正式 GA 结论。

## 当前实现面

| 模块 | 当前实现 |
| --- | --- |
| `orchestrator-core` | 纯领域模型、Release v2、ApiBinding、确定性 plan/diff、状态机和 published action；不持有数据库、文件、网络、进程或 Docker I/O。 |
| `orchestrator-storage` | Memory、SQLite、PostgreSQL 三后端契约；持久化 Operation/Job/Topology/RuntimeInstance、ApiBinding 和 RuntimeReport；SQLite 使用 WAL/外键/busy timeout/旁文件锁，PostgreSQL 使用 TLS pool、schema checksum 和单主动 advisory lock。 |
| `orchestrator-control-plane` | 持久 Operation/Job、attempt/event、lease CAS、心跳、重试、取消、恢复、补偿、Binding apply saga 和 reconciler。 |
| `orchestrator-runtime` / `orchestrator-agent` | Node mTLS pull、本地 SQLite ledger、真实 Docker facts、Deployment 专属只读 ServiceContext、15 分钟 workload JWT 轮换、固定 runtime profile 和 Docker Engine API 执行；不接受任意 shell。 |
| `orchestrator-manager` / Store | Catalog v2、Release v2 lint/导入、provider 候选、显式 Binding 选择、节点放置、安装/升级/回滚/卸载、真实健康提升和失败补偿。 |
| Topology | 不可变 Revision、Endpoint/Link/ApiBinding、强 ETag、确定性 diff、validate/apply/rollback、staged Binding、正式 Status 和 drift。 |
| Gateway/Auth | 控制面事务持久化 API surface；Gateway 以 consumer Deployment + API ID 路由并实时校验 Binding/generation；Auth 签发每 Deployment 独立的 Ed25519 JWT，不回退 admin bearer。 |
| Problem/Judge | Problem 确定性题包与内容寻址 artifact、事务 outbox、CloudEvents snapshot/tombstone、Judge inbox/projection/backfill；Submission 固定题包 revision，Worker 只经 Gateway 长轮询和下载 `ApiResourceRef`。 |
| Judge Worker | 签名 `judge-sandbox-v1` 固定 HostConfig、Agent 本地 policy、Docker HEALTHCHECK、live/ready/healthcheck、资源 SHA-256/size 校验和重连恢复。 |
| Desktop / Web / TUI | Tauri WebView 内嵌 backend/loopback Agent；远程 OIDC Web 与 Device Flow TUI；Store Binding 选择、Topology Binding、Deployment runtime/context/health 和 Operation 控制使用同一 `/api/v1` 契约。 |

0.2 Console、旧 PostgreSQL 仓储、共享 worker token、Docker Compose/本地进程适配和旧路由被隔离在开发兼容或 `orchestrator-legacy` 路径，不是 v1 生产路径。

## A/B 跨机形态

- A 机运行单主动 Orchestrator、Gateway、Auth、Problem、Judge API、Storage、Redis 和 PostgreSQL。
- B 机运行已注册 Agent，以及由 Store 部署的 Judge Worker。Agent/宿主访问控制面和 OCI registry；Worker 业务网络只访问 A 的 HTTPS Gateway，不直连 A 的 PostgreSQL、Redis、MinIO 或 Judge API 端口。
- Release 只声明 `provides.apis`、`requires.apis`、events 和不可变 runtime contract。Topology Link 的 `api_bindings` 决定 consumer/provider；业务代码只使用 requirement 名和 Service SDK。
- 断开 Link、卸载 Deployment 或切换 provider 会提升 credential generation；旧 JWT 即使未过期也不能命中活动路由。

完整契约见 [Service Contract v2](orchestrator/service-contract-v2.md)，B 节点操作见 [Judge Worker 生产部署](../deploy/worker/README.md)。

## 本地 pre-commit 双 Engine 证据

| 字段 | 值 |
| --- | --- |
| 证据文件 | `artifacts/cross-machine/full-evidence.json` |
| mode/status | `full-components` / `PASSED` |
| run_id | `65b49b3919` |
| 执行时间 | 2026-08-10 04:15:44Z 至 04:37:53Z |
| 证据文件 SHA-256 | `c8ef2278125ae2c146895acad29a12337e3e16187f83565dc564512ea93c3f9f` |
| 报告内 build identity | `52d67919231f663411698af625284def4f9ccccd` / `production` / `x86_64-unknown-linux-gnu` |
| 环境 | 单台 Linux 测试环境中的两个 privileged Docker-in-Docker Engine；不是两台物理主机 |

这是 **pre-commit 工作树证据**。报告内 commit 是运行时注入的基线身份，不能覆盖未提交文件，也不能证明最终候选 commit 与该工作树字节一致。最终功能候选必须先冻结 clean commit，再从该 commit 重跑相同 `--full-components` 门禁并保存新的 run_id、SHA-256 和 CI/checkout 身份。命令、验证器和证据范围见 [跨机门禁说明](../deploy/cross-machine/README.md)。

## 尚未取得、不得声称

- 最终 clean candidate commit 对应的双 Engine `full-components` 证据；
- 两台物理 A/B 主机或真实跨地域网络的部署证据；
- 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operations和连续 24 小时稳定性证据；
- Windows/Linux 受信任签名 GA 制品、正式 tag 或 GitHub Release；
- 最终依赖、权限、鉴权和供应链安全验收。

这些未取得的证据不抹去上面的本地功能结果，但必须在对外作相应声明前单独完成。额外门禁定义见 [生产就绪证据](production-readiness.md)。

## 固定产品边界

v1 固定为单主动控制面、显式节点放置和 Docker Engine；不实现 active-active HA、自动 failover、自动扩缩容、通用调度器、Kubernetes runtime、多租户计费或任意本地命令执行。IOI 赛制、赛时计分、排行榜和重测策略仍属于后续业务目标。
