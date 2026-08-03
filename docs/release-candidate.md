# Orchestrator v1.0 发布候选判定

## 当前结论：NO-GO

当前源码已经达到 v1 功能候选状态，发布 workflow 也已编码功能、升级、容量和制品门禁；但尚无一个冻结 commit 同时满足全部外部证据。因此不能创建或宣称 `v1.0.0` GA。

`Cargo.toml`、Desktop、Web 和 Service/Release manifest 中的 `1.0.0` 是候选版本一致性要求，不是发布证明。历史 `v0.1.0-alpha` 及任何旧 commit 的绿色 workflow 不能替代当前候选。

## 功能候选判定

| 范围 | 当前判定 | 说明 |
| --- | --- | --- |
| 正式契约 | 已实现 | `/api/v1`、OpenAPI、published action/RBAC、Problem Details、幂等、cursor、ETag 和 SSE 已进入自动契约。 |
| 持久化与并发 | 已实现 | Desktop SQLite、生产 PostgreSQL TLS pool、schema checksum、单主动锁、持久 Job/Operation、恢复和有界过载均有实现与契约测试。 |
| Node/runtime | 已实现 | mTLS pull Agent、本地 ledger、Docker Engine API、完整 deployment 生命周期及崩溃恢复路径已有测试入口。 |
| Store | 已实现 | Catalog v2、OCI digest、安装默认启动、健康提升、升级/回滚/卸载、补偿和 provider fail-fast 已接入 v1。 |
| Topology | 已实现 | immutable revision、确定性 diff、validate/apply/rollback、Status/drift、并发冲突和 UI state 已接入 v1。 |
| Desktop/Web/TUI | 已实现 | Tauri 内嵌 WebView/loopback backend/Agent，远程 OIDC Web、Device Flow TUI 和 published action 能力等价已经落地。 |
| 运维与发布工具 | 已实现 | preflight、live/ready、指标、日志保留、备份恢复、容量 runner、兼容构建和多平台打包/签名 workflow 已存在。 |

“已实现”仅表示代码和自动化入口具备，仍须由待发布 commit 的 CI 与候选环境复验。

## 阻止 GO 的两项外部证据

| 门禁 | 当前状态 | GO 条件 |
| --- | --- | --- |
| 同 commit 生产规模 + 24h | 缺失 | production profile 报告证明 100 Nodes、2,000 Deployments、10,000 Endpoint+Link、50 并发 Operations、全部 p95/恢复阈值及完整 24 小时稳定性，并通过 commit 绑定校验。 |
| 签名多平台 GA 制品 | 尚未生成/发布 | Windows MSI/ZIP 与 Linux DEB/AppImage/tar.gz 及 SHA256、SPDX SBOM、provenance、Sigstore bundle、attestation 全部从同一候选 commit 生成并通过布局 smoke。 |

0.2.0 → 1.0.0 真实升级不再是外部未实现项。发布 contract gate 使用历史 0.2 仓储 writer
向真实 TLS PostgreSQL 17 旧表写入数据，再由 v1 验证 migration/import、未应用 draft、
`External/Unknown` runtime 和重启幂等；数据库与 artifact 联合备份恢复也有真实 drill。
这些门禁已在本地通过，但候选 commit 仍须在 release CI 中重跑，任何失败都会阻止 GA build。

## 晋级顺序

1. 冻结唯一候选 commit；源码、schema、lockfile、workflow 和发布文档均纳入该 commit。
2. 在该 commit 上运行 release contract/function gates，包括真实 Docker Agent 生命周期和 Web 30 分钟持续运行。
3. 在同一 commit 上取得 production 100/2000/10000/50 + 24h 报告，并由 release workflow 重新校验。
4. 确认该 commit 的真实持久 0.2.0 → 1.0.0 升级与联合备份恢复 gate 通过；若失败，修复后生成新候选 commit，并从第 2 步重跑。
5. 运行 Windows/Linux GA build，下载后复核资源布局、checksum、签名、SBOM、provenance 和 attestation。
6. 仅在上述结果全部属于同一 commit 时创建并验证 `v1.0.0` tag，发布不可变 GA assets，并把本页改为 GO，登记 run ID 和 artifact digest。

## v1 发布声明边界

即使晋级为 GO，v1 仍只声明单主动控制面、最多 100 Nodes、Docker Engine、显式节点放置和单租户运维边界；不声明 active-active、自动 failover、自动扩缩容、Kubernetes runtime、通用调度器或多租户计费。

详细证据要求见 [生产就绪证据](production-readiness.md)，实际运维命令见 [Orchestrator v1.0 运维手册](orchestrator/operations-v1.md)。
