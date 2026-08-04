# Orchestrator v1.0 可核对证据索引

本页说明“到哪里核对实现和自动化”，不记录历史绿色 run。本地功能与 unsigned portable 的交付结论见[交付判定](../release-candidate.md)；需要额外声明生产规模或受信任签名分发时，再按[生产就绪证据](../production-readiness.md)把对应 artifact 与 commit 绑定。

## 实现入口

```text
services/orchestrator/core/          纯领域模型、校验、plan/diff、published action
services/orchestrator/storage/       Memory / SQLite / PostgreSQL、迁移、锁和持久投影
services/orchestrator/control-plane/ Operation / Job / lease / recovery / compensation
services/orchestrator/runtime/       Docker Engine 与类型化运行时契约
services/orchestrator/manager/       Catalog / Release / Store 用例
services/orchestrator/agent/         mTLS pull Agent、本地 execution ledger
services/orchestrator/backend/       /api/v1、身份、SSE、live/ready/metrics
manager/desktop/                     Tauri WebView、embedded backend/Agent、SQLite
manager/web/                         Web 控制面
manager/tui/                         /api/v1 Device Flow 客户端
platform/schemas/orchestrator/       OpenAPI、published action、Agent protocol
```

`services/orchestrator/legacy/` 是 0.2 兼容边界，不应被引用为 v1 默认持久化、Node 或 provider 语义。

## 功能证据

### 正式契约

- `services/orchestrator/core/src/contract_v1.rs` 校验 OpenAPI、published action 和类型契约。
- backend contract tests 校验 router、dispatcher、RBAC、capabilities、Problem Details、request id、幂等、cursor、ETag 和 SSE。
- `manager/web/src/published-actions.ts` 与 `manager/tui/src/remote.rs` 由能力矩阵测试和共同 fixture 锁定；发布矩阵不得包含 `UNSUPPORTED`。

### 存储、并发与恢复

- `services/orchestrator/storage/tests/sqlite_contract.rs`：WAL、foreign keys、busy timeout、schema checksum、文件锁和重启持久化。
- `services/orchestrator/storage/tests/postgres_contract.rs`：证书校验 TLS pool、readiness、schema checksum、单主动 advisory lock、事务仓储、retention、旧数据一次性导入和重启幂等。
- `services/orchestrator/control-plane/` tests：32 并发 claim、lease epoch/CAS、heartbeat、重复 complete、乱序 event、retry、cancel、过期恢复和 `NEEDS_ATTENTION`。
- backend durable/recovery tests：SQLite/PostgreSQL 重新打开后恢复 Operation/Job，慢 I/O 不占数据库事务或全局 console mutex。

### Node、Runtime 与 Store

- `services/orchestrator/agent/`：一次性 enroll、证书 generation/续签/激活/吊销、本地 SQLite ledger、重复投递和 SIGTERM 排空。
- `services/orchestrator/runtime/`：Docker socket/pipe 请求、固定动作、artifact digest/RepoDigest 校验、取消和错误映射。
- `services/orchestrator/backend/tests/docker_agent_v1_e2e.rs` 与 `deploy/ops/orchestrator-docker-agent-e2e.sh`：真实 registry、Docker Engine、Agent pull Job，以及 install/start/stop/restart/uninstall 和重启恢复。
- `services/orchestrator/manager/` 与 Store API tests：Catalog v2、签名/依赖/平台、仅导入零 Docker、默认安装 Running、幂等、失败补偿、升级/回滚、External health 和 provider fail-fast。

### Topology

- storage topology contracts：不可变 revision、强 ETag、确定性 diff、draft/applied head、Status 与 rollback 新 revision。
- control-plane/backend tests：validate、apply saga、补偿、并发冲突、drift/reconciler、审计和恢复。
- `manager/web/e2e/orchestrator-v1.spec.ts`：Endpoint/Link draft 编辑、validate/diff/apply/rollback、Status、drift 和布局失败可见。

### Desktop、Web 与 TUI

- `manager/desktop` tests：随机 loopback backend、应用数据目录 SQLite、bootstrap exchange、导航限制、shutdown 和资源发现；release gate 还以 `OJOS_DESKTOP_SMOKE_DURATION_MS=1800000` 在真实 Tauri WebView 中连续验证认证 API、应用 shell 和事件循环。
- Web Vitest/Playwright：Store、Topology、Deployment、Node、Operation/SSE、Diagnostic、RBAC、Problem、失败补偿和布局；`OJOS_E2E_SOAK_MS=1800000` 用于 30 分钟持续运行门禁。
- TUI tests：Device Flow、capabilities、完整 published command surface、mutation idempotency、cursor、ETag、Problem 和事件语义；`--legacy-local` 只属于显式 0.2 兼容。

### 运维与制品

- `deploy/ops/orchestrator-preflight.sh`：生产 PostgreSQL/TLS/OIDC/Node CA/Catalog/artifact/Web build 配置 fail closed。
- `deploy/ops/orchestrator-backup.sh`、`orchestrator-restore.sh`：PostgreSQL 与 OCI artifact 的一致备份、checksum 和失败切回。
- `deploy/ops/orchestrator-capacity-gate.py`：100/2000/10000/50、真实重启、延迟和 24 小时稳定性；通过受保护的无 shell helper 在 qualification、每个 Operation round 和 final 记录独立 Engine/容器/网络环境证据 sidecar。
- `deploy/ops/validate-orchestrator-ga-evidence.py`：拒绝错误 commit、smoke、短时或阈值不合格报告。
- `.github/workflows/release.yml`：手工创建不可变签名候选，拒绝 workflow rerun，串联功能/升级门禁、同 commit production evidence、多平台构建、Azure OIDC Artifact Signing、布局 smoke、SHA256、SPDX SBOM、provenance、逐主制品 Sigstore 和 attestation；上传后用 Actions API 固定 artifact ID/digest 并生成独立候选身份记录；tag 不触发该 workflow。
- `deploy/release/orchestrator-candidate.py`：锁定首次 workflow attempt、11 个主制品、11 个 Sigstore bundle 和精确 22 文件 payload，验证结构化 Authenticode/容量身份，并生成 `SECURITY_ACCEPTANCE_PENDING`、`published=false` 的 schema v2 证据 manifest；晋级时核对受保护的 run/manifest/artifact 身份。
- `deploy/release/authenticode-timestamp.ps1` 与 `verify-windows-authenticode.ps1`：从嵌入签名解析并验证 OJOS 制品的 RFC3161/SHA-256 token 与父签名绑定，如实记录 Microsoft WebView2Loader 的原始 timestamp 协议。
- `deploy/release/verify-orchestrator-v1-trust.sh`：晋级前锁定首次 candidate attempt，并逐一复验 11 个主制品的 manifest digest、Cosign 身份、GitHub attestation 与平台 checksum。
- `deploy/ops/tests/test_release_workflow_policy.py`：静态锁定候选/晋级互斥、无 tag trigger、Azure 签名顺序、无重建晋级及 manifest 不进入 Release 资产。
- `.github/workflows/orchestrator-candidate-images.yml`：从 `main` 同一 SHA 构建 control-plane、Agent、capacity fixture 三个 digest 候选镜像；fixture base 必须 digest 固定，每个镜像都写 revision label、启用 BuildKit provenance/SBOM、生成 GitHub attestation，并在拉回核对后上传 digest evidence。

## 本地功能复核

下列命令适合开发阶段发现回归；它们不产生 GA 运行证据：

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets

Push-Location manager/web
npm ci
npm run typecheck
npm test
npm run build
npm run test:e2e
Pop-Location

python -m unittest discover -s deploy/ops/tests -p "test_*.py"
git diff --check
```

TLS PostgreSQL contract 需要专用数据库和 `OJOS_TEST_POSTGRES_URL`/`OJOS_TEST_POSTGRES_CA`。真实 0.2 writer 导入还需要单独设置 `OJOS_TEST_POSTGRES_UPGRADE_URL`；若环境变量缺失，对应测试会跳过，不能把整个命令的成功误写为该演练已完成。

Docker Agent 生命周期需要真实 Docker daemon/registry。Web 30 分钟持续运行要显式设置 `OJOS_E2E_SOAK_MS=1800000`，Desktop 30 分钟持续运行要同时使用 `OJOS_DESKTOP_SMOKE=1` 与 `OJOS_DESKTOP_SMOKE_DURATION_MS=1800000`；普通短时 E2E 或仅启动一次 WebView 都不能替代。

生产容量还必须使用 `orchestrator-candidate-images.yml` 从候选 `main` commit 产生的三个
RepoDigest。候选镜像 artifact 中的 commit、digest、revision label 和 workflow run 是镜像
身份入口；容量报告仍须把 `source_commit`、`oci_revision`、`provenance_commit` 和
`server_build.commit_sha` 绑定到同一 SHA。自行重打 tag 或只记录镜像名不构成有效证据。

## 当前不能从仓库内测试推出的结论

即使以上测试全部通过，也仍不能声称：

- 当前候选已在 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operation 下持续运行 24 小时；
- Windows/Linux GA 安装包和 portable 包已经从同一候选 commit 实际生成、签名、attest 并下载复核；
- 候选已经完成最终安全验收或已经发布。

真实持久 0.2.0 → 1.0.0 导入和 PostgreSQL/artifact 联合恢复已经进入自动 release gate，
候选 commit 仍须重跑，但不再列为外部功能缺口。两个工程外部证据的完成标准见
[未完成的上线证据](../unfinished/README.md)；候选与晋级边界见
[签名候选与晋级政策](candidate-promotion.md)。当前状态固定为
`SECURITY_ACCEPTANCE_PENDING`、`published=false`，不得发布。
