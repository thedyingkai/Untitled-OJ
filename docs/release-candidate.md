# Orchestrator v1.0 交付判定

## 当前结论：本地功能 GO

当前源码、自动化测试和原生 portable 安装入口已经达到 v1 本地功能交付状态。Windows/Linux 用户从解压后的 portable 包直接运行 `ojos-orchestrator install`，不需要安装脚本、MSI、Azure 签名、专用 runner 或 10×10 主机群。

`Cargo.toml`、Desktop、Web 和 Service/Release manifest 中的 `1.0.0` 是版本一致性要求。是否创建 tag 或 GitHub Release 是单独的发布决定，不影响本页对当前代码功能的判定。

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

“已实现”表示代码、自动化入口和本地 portable 运行链路具备；候选 commit 推送后仍须由功能 CI 复验，未产生的 hosted CI 结果不在本页提前声明。

## 可选的额外生产证据

| 证据 | 当前状态 | 何时需要 |
| --- | --- | --- |
| 100 Node + 24h | 未运行 | 只有对外声明“已验证 100 Node 生产容量”时才需要；不用于判断 API、Store、Topology、Agent 或 Desktop 是否正确。 |
| MSI/Authenticode/Sigstore | 未运行 | 只有公开分发、企业签名策略或发布者信任需要时才执行；unsigned portable 与命令行安装不依赖它。 |

0.2.0 → 1.0.0 真实升级不再是外部未实现项。发布 contract gate 使用历史 0.2 仓储 writer
向真实 TLS PostgreSQL 17 旧表写入数据，再由 v1 验证 migration/import、未应用 draft、
`External/Unknown` runtime 和重启幂等；数据库与 artifact 联合备份恢复也有真实 drill。
这些门禁已有本地演练入口和历史通过记录；候选 commit 的结果以对应 CI run 为准。

## 普通 portable 交付

1. 提交前在本地运行 Rust、Web、TUI、Desktop 和安装布局测试；真实 Docker Agent、PostgreSQL 由可用环境下的合同门禁覆盖；提交后的候选 commit 再由对应 CI 复验。
2. Windows/Linux 都运行包内的原生 `ojos-orchestrator install`；从无关工作目录验证原生主命令、四个程序、真实 daemon/Web UI 和 Desktop embedded smoke。
3. `.github/workflows/orchestrator-portable.yml` 生成包含原生 installer、逐文件 manifest 和 payload 的 unsigned ZIP/tar，并在重新解压后的干净目录再次 verify；它既不构建 MSI，也不请求云签名或容量环境。
4. 如未来确实需要公开签名 GA，再单独运行保留的容量与 signed-GA 流程，不反向改变普通功能结论。

## v1 发布声明边界

即使晋级为 GO，v1 仍只声明单主动控制面、最多 100 Nodes、Docker Engine、显式节点放置和单租户运维边界；不声明 active-active、自动 failover、自动扩缩容、Kubernetes runtime、通用调度器或多租户计费。

详细证据要求见 [生产就绪证据](production-readiness.md)，实际运维命令见 [Orchestrator v1.0 运维手册](orchestrator/operations-v1.md)。
