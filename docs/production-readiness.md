# Orchestrator v1.0 生产就绪证据

本文只定义需要生产规模声明或受信任公开分发时使用的额外证据，不维护功能状态。当前实现与本地证据只以[项目状态总结](completeness-summary.md)为准；源码、lockfile、schema、workflow 或发布文档变化后，旧证据不能代表新 commit。

## 证据边界

本地 `full-components` 双 Engine 运行属于功能证据，不是本页定义的 100 Node/24 小时、物理双主机、签名 GA 或安全验收。其 run_id、文件 SHA-256 和 pre-commit 限制统一记录在[项目状态总结](completeness-summary.md)，本页不复制当前结论。

历史 `v0.1.0-alpha` 和旧 `2a0d647`/`875586f` workflow 记录只证明当时的代码，不能作为当前候选证据。`docs/evidence/*.json` 同样是历史快照。

## 已落地的生产基础

| 范围 | 当前实现 | 可核对入口 |
| --- | --- | --- |
| 存储 | Desktop 使用 SQLite；远程 daemon 强制 PostgreSQL。SQLite 启用 WAL、外键、busy timeout 和旁路文件锁；PostgreSQL 使用 r2d2 连接池、证书校验 TLS、迁移 checksum 和专用 advisory-lock 连接。两者失败均不回退内存。 | `services/orchestrator/storage/`、`sqlite_contract.rs`、`postgres_contract.rs` |
| 并发与恢复 | 持久 Operation/Job、lease CAS、heartbeat、retry、event 去重、启动恢复、SIGTERM 最长 30 秒排空；结果不可证明时进入 `NEEDS_ATTENTION`。事务只包状态转换，下载、Docker、探测和 provider I/O 在事务外。 | `services/orchestrator/control-plane/`、backend 恢复测试 |
| Node/runtime | 一次性注册码兑换 SPIFFE Node ID 的 mTLS 证书，30 天有效、提前 7 天续签、可吊销；Node 长轮询领取本节点 Job，本地 SQLite ledger 保证幂等；Docker 走 Engine API socket/pipe 并核对 RepoDigest。 | `services/orchestrator/agent/`、`services/orchestrator/runtime/`、`backend/tests/docker_agent_v1_e2e.rs` |
| 跨节点 Service | Release v2、ApiBinding、ServiceContext、RuntimeReport、短期 workload JWT、固定 runtime profile 和 Gateway consumer 路由；Problem/Judge 通过 outbox/inbox 与 ApiResourceRef 解耦。 | `docs/orchestrator/service-contract-v2.md`、`deploy/cross-machine/` |
| Store | Catalog v2 信任、依赖/平台/版本选择、OCI digest、release-version 签名能力、精确 endpoint/port 绑定、导入与 Managed/External 安装、升级/回滚/卸载、健康投影和补偿；缺少 provider 时 plan 失败。 | `services/orchestrator/manager/`、Store API/contract tests |
| Topology | 不可变 revision、强 ETag、确定性历史 diff、按精确 Runtime release 绑定的真实 Endpoint/Link probe、validate/apply/rollback saga、Status、drift、审计和按用户布局。 | storage topology contract、backend API/probe tests、Web Playwright |
| 身份与 API | `/api/v1`、Problem Details、request id、Idempotency-Key、cursor、SSE；远程 Web OIDC Code + PKCE、TUI Device Flow、viewer/operator/admin RBAC、append-only audit。 | OpenAPI/action contract、backend auth tests、Web/TUI fixtures |
| Desktop/UI | Tauri WebView 内嵌同源 Web UI 与随机 loopback backend、HttpOnly bootstrap session；本机 managed execution 明确 `Unavailable`，须使用独立 Agent；Web/TUI published action 对齐，持续运行测试可配置到 30 分钟。 | `manager/desktop/`、`manager/web/`、`manager/tui/` |
| 运维 | fail-closed preflight、live/ready、Prometheus、可选 OTLP、日志保留、PostgreSQL + artifact 备份恢复、容量/恢复/soak runner。 | `docs/orchestrator/operations-v1.md`、`deploy/ops/` |

这些条目说明实现与可重复测试入口已经存在；它们不替代候选环境的实际运行 artifact。

## 可选 signed-GA 门禁现状

`.github/workflows/release.yml` 已把以下步骤串成依赖图：

```text
contract-gates
├─ v1 contracts / SQLite / TLS PostgreSQL / control-plane / runtime / manager / agent
├─ real Docker registry + Agent Store Job lifecycle
├─ Web/TUI contract + 30-minute browser soak
└─ operations/release script validation
        │
        ├─ 0.2.0 compatibility artifacts
        │       └─ upgrade-drill
        └─ same-commit production evidence
                └─ Windows/Linux GA build, sign, attest and publish
```

该依赖图只用于选择 signed-GA 发行方式，会阻止缺少 production evidence 的签名构建。普通 `.github/workflows/orchestrator-portable.yml` 不进入该依赖图，只构建、安装、启动 smoke 并上传 unsigned ZIP/tar 与 SHA256。

## 按需执行的外部证据

### 1. 生产规模与 24 小时稳定性

必须在同一候选 commit 上完成 `.github/workflows/orchestrator-capacity.yml` 的 `production` job。有效报告至少证明：

- 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operations；
- 读 p95 ≤ 200 ms、异步 mutation 接受 p95 ≤ 500 ms、事件 p95 ≤ 1 秒；
- 真实控制面重启后 ≤ 60 秒 ready，重启前持久 Operation 仍存在；
- 连续运行 ≥ 24 小时，且无永久 Operation、失联 lease、连接/线程泄漏，暖机后 RSS 增长 < 10%；
- 首次 workflow attempt 的环境 sidecar 完整覆盖 qualification、每个 Operation round 和 final，
  证明 10 worker×10 独立 Engine、2,000 个真实运行/健康容器及无 drift 网络资源身份始终一致。

`release.yml` 会下载 production artifact，并用 `deploy/ops/validate-orchestrator-ga-evidence.py` 重新核对 commit、profile、规模、时长和阈值。smoke 或其他 commit 的报告会被拒绝。

### 2. 签名多平台 GA 制品

GA build 必须实际产出并验证 Windows x64 的 MSI/portable ZIP，以及 Linux x86_64 的 DEB/AppImage/tar.gz；每个平台必须同时有 SHA256SUMS、SPDX SBOM、provenance、逐文件 Sigstore bundle 和 build provenance attestation。Windows 四个 EXE 和 MSI 必须验证结构化 RFC3161/SHA-256 timestamp token，`WebView2Loader.dll` 保留并如实报告 Microsoft 原签名。安装版与 portable 包必须分别通过独立启动 smoke，且 Desktop 不依赖仓库当前目录、不打开外部浏览器。

在这些 artifact 生成、重新下载验证并与候选 commit/tag 对齐前，不得声称已经 GA。

## 已闭环、候选时自动复验的升级门禁

`release.yml` 的 contract gate 会在真实 TLS PostgreSQL 17 中应用 0.2 schema，再由从历史
`PgOrchestratorStore` 提取的独立 writer 写入旧 HostService、Endpoint、Link、Topology snapshot
和 runtime 数据。随后 v1 仓储执行 migration/import，并验证：

- 旧 Topology 只导入为未应用 draft；
- 旧 runtime 只投影为 `External/Unknown`，不伪造 digest 或运行态；
- 第二次打开数据库不会重复创建 revision 或 runtime；
- 旧数据确实在导入前由 0.2 writer 写入，而不是由 v1 测试伪造。

同一发布门禁还使用 PostgreSQL 17 客户端执行数据库与 artifact 联合备份、校验、篡改后恢复、
必需表核对和旧 artifact 保留。以上路径已在本地真实 PG17/TLS 环境通过；候选 commit 仍必须在
CI 中重新运行并保留 artifact，本地结果不替代候选结果。

## 证据登记规则

每次候选运行至少记录：

- 完整 commit SHA、workflow run ID/attempt、触发方式和时间；候选与容量证据只接受首次 attempt；
- 使用的 profile、规模、持续时间和阈值；
- artifact 名称与 SHA-256；签名候选还要登记 candidate manifest SHA-256、Actions artifact ID、REST `artifact.digest` 和 upload action digest；
- 所有跳过、重试和失败步骤；
- 若只修改证据文档，明确写出它所描述的代码 SHA，不能把文档 commit 冒充被测 commit。

本地功能与 unsigned portable 的交付结论维护在[交付判定](release-candidate.md)；本页只记录按需执行的生产规模与签名证据。门禁命令和故障处置见 [v1 运维手册](orchestrator/operations-v1.md)。
