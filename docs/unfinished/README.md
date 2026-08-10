# Orchestrator v1.0 可选的上线证据

本页只列出未来作特定生产规模或签名发行声明时需要的额外证据，不维护功能完成状态。当前实现、Service Contract v2 的本地双 Engine 结果及其限制只以[项目状态总结](../completeness-summary.md)为准。

下列两项不再阻塞本地功能完成、unsigned portable 构建或命令行安装。

## 1. 同一候选 commit 的生产规模与 24 小时稳定性

使用 `.github/workflows/orchestrator-capacity.yml` 的 `production` profile，在带 `orchestrator-soak` 标签的专用 runner 上，连接已经准备好的单主动生产形态控制面。环境和报告必须同时满足：

- 至少 100 Nodes；
- 至少 2,000 Deployments；
- Endpoint + Link 总数至少 10,000；
- 50 个并发 Operation；
- 连续运行至少 86,400 秒；
- 读 p95 ≤ 200 ms，异步 mutation 接受 p95 ≤ 500 ms，SSE 事件 p95 ≤ 1 秒；
- 控制面真实重启后 60 秒内恢复 ready，重启前持久 Operation 仍可读取；
- 无永久 RUNNING/ENQUEUING/CANCELLING Operation、无失去心跳的 LEASED Job、无过期 lease；
- 暖机后 RSS 最大增长 < 10%，线程数无持续泄漏。

完成条件：`deploy/ops/validate-orchestrator-ga-evidence.py` 对 production 报告和候选 `GITHUB_SHA` 校验通过，报告与相关日志作为 workflow artifact 保存。smoke、其他 commit、负载均衡地址或手工填写的结论均不能替代。

## 2. 签名的多平台 GA 制品

`.github/workflows/release.yml` 已定义构建、布局 smoke、签名、attestation 和发布步骤，但尚未因此自动拥有 GA 制品。

完成条件：同一候选 commit 在全部前置门禁通过后实际生成并验证：

- Windows x64：MSI、portable ZIP；
- Linux x86_64：DEB、AppImage、portable tar.gz；
- 每个平台的 SHA256SUMS、SPDX SBOM、provenance；
- 每个发布文件对应的 Sigstore bundle，以及 GitHub build provenance attestation；
- 安装版与 portable 布局均能在不依赖仓库工作目录、且不传 `--repo-root`/`--web-root` 的情况下启动 Desktop 并完成 WebView/bootstrap/API smoke。

最终发布前还要核对 `v1.0.0` tag 与构建 commit 一致，下载后的 checksum、签名和 provenance 可重新验证。

## 功能状态不在本页维护

- Desktop 默认 SQLite；生产 daemon 默认 PostgreSQL，内存只允许显式 `--ephemeral`。
- PostgreSQL 连接池、证书校验 TLS、schema checksum、单主动 advisory lock；SQLite WAL/外键/busy timeout/文件锁。
- v1 持久路径没有全表内存镜像和全局 console mutex，慢 I/O 不在数据库事务内执行。
- Node 使用一次性注册码、SPIFFE ID mTLS 证书和 pull Job；Deployment 使用短期 workload JWT 与只读 ServiceContext，旧 push/shared-token 路径不属于 v1。
- Store 缺少 provider 时在 plan 阶段失败，不返回 Deferred；Managed install 默认启动并以真实健康结果提升投影。
- Topology 使用不可变 Revision、确定性 diff、apply/rollback saga、Status 与 drift。
- Desktop 使用 Tauri WebView 内嵌同源 Web UI，不打开外部浏览器；TUI 是远程 `/api/v1` 客户端，不在进程内执行 mutation。
- Web/TUI 使用 OIDC/RBAC/审计与同一 published action 契约。
- Release v2、ApiBinding、`judge-sandbox-v1`、Problem→Judge outbox/projection 和 Gateway-only Worker 的当前证据边界见唯一状态源；本页不重复判定。
- 真实 0.2 → v1 持久升级由历史仓储 writer、TLS PostgreSQL 17 和 v1 导入契约验证，
  数据库/artifact 联合备份恢复另有真实 drill；`release.yml` 会在每个候选 commit 上自动重跑。

实现状态摘要见 [项目状态总结](../completeness-summary.md)，发布判定见 [发布候选判定](../release-candidate.md)。
