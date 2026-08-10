# Orchestrator v1.0 交付判定

## 状态来源

本页只定义候选判定规则，不维护第二份功能状态。当前实现、pre-commit 双 Engine run_id/证据 SHA-256、证据限制和未完成项只以[项目状态总结](completeness-summary.md)为准。

Windows/Linux 用户可以从解压后的 portable 包直接运行 `ojos-orchestrator install`；这条安装路径不依赖安装脚本、MSI、Azure 签名、专用 runner 或 10×10 主机群，但不能替代最终候选 commit 的功能复验。

`Cargo.toml`、Desktop、Web 和 Service/Release manifest 中的 `1.0.0` 是版本一致性要求。是否创建 tag 或 GitHub Release 是单独的发布决定，不能改变或替代状态源记录的功能证据。

## 功能候选复验范围

| 范围 | 候选必须证明 |
| --- | --- |
| 正式契约 | `/api/v1`、OpenAPI、published action/RBAC、Problem Details、幂等、cursor、ETag 和 SSE 自动契约通过。 |
| 持久化与并发 | Desktop SQLite、生产 PostgreSQL TLS pool、schema checksum、单主动锁、持久 Job/Operation、恢复和有界过载门禁通过。 |
| Node/runtime | mTLS pull Agent、本地 ledger、Docker Engine API、完整 Deployment 生命周期及逐点崩溃恢复通过。 |
| Store/Topology | Catalog v2、OCI digest、安装健康提升、升级/回滚/卸载/补偿，以及 immutable revision、diff/apply/rollback、Status/drift 和并发冲突通过。 |
| Service Contract v2 | Release v2、ApiBinding、ServiceContext、Deployment JWT、runtime facts/profile、Problem→Judge 投影和 Gateway-only Worker 在最终 clean commit 的 `full-components` 双 Engine 门禁通过。 |
| Desktop/Web/TUI | Tauri embedded smoke、远程 OIDC Web、Device Flow TUI、持续运行和 published action 能力等价通过。 |
| 运维与 portable | preflight、live/ready、指标、日志保留、备份恢复、兼容升级，以及 Windows/Linux portable 解压、安装、独立目录启动通过。 |

各项当前结果只查阅状态源。最终候选必须在 clean checkout 上复跑合同、双 Engine、UI/TUI、Docker Agent、PostgreSQL 升级/恢复和 portable 门禁；未产生的结果不能提前登记。

## 可选的额外生产证据

| 证据 | 何时需要 |
| --- | --- |
| 100 Node + 24h | 只有对外声明“已验证 100 Node 生产容量”时才需要；当前是否已有证据查状态源。 |
| MSI/Authenticode/Sigstore | 只有公开分发、企业签名策略或发布者信任需要时才执行；当前是否已有制品查状态源。 |

0.2.0 → 1.0.0 候选复验使用历史 0.2 仓储 writer
向真实 TLS PostgreSQL 17 旧表写入数据，再由 v1 验证 migration/import、未应用 draft、
`External/Unknown` runtime 和重启幂等；数据库与 artifact 联合备份恢复也有真实 drill。
候选 commit 的结果必须以对应 clean checkout/CI run 为准，历史记录只用于排障。

## 普通 portable 交付

1. 提交前在本地运行 Rust、Web、TUI、Desktop 和安装布局测试；真实 Docker Agent、PostgreSQL 由可用环境下的合同门禁覆盖；提交后的候选 commit 再由对应 CI 复验。
2. Windows/Linux 都运行包内的原生 `ojos-orchestrator install`；从无关工作目录验证原生主命令、四个程序、真实 daemon/Web UI 和 Desktop embedded smoke。
3. `.github/workflows/orchestrator-portable.yml` 生成包含原生 installer、逐文件 manifest 和 payload 的 unsigned ZIP/tar，并在重新解压后的干净目录再次 verify；它既不构建 MSI，也不请求云签名或容量环境。
4. 如未来确实需要公开签名 GA，再单独运行保留的容量与 signed-GA 流程，不反向改变普通功能结论。

## v1 发布声明边界

即使晋级为 GO，v1 仍只声明单主动控制面、最多 100 Nodes、Docker Engine、显式节点放置和单租户运维边界；不声明 active-active、自动 failover、自动扩缩容、Kubernetes runtime、通用调度器或多租户计费。

详细证据要求见 [生产就绪证据](production-readiness.md)，实际运维命令见 [Orchestrator v1.0 运维手册](orchestrator/operations-v1.md)。
