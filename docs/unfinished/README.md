# 未完成事项

这里记录已经确认、但本轮没有收尾的工作。条目按“需要先做决策”和“可以直接实现”分开，避免把架构选择伪装成几个小时就能修完的小问题。

## 需要先做决策

- 完整的 operator 认证模型：Web UI 目前使用共享的 `ORCHESTRATOR_INTERNAL_TOKEN`，TUI 则在进程内调用 core。两者都没有 operator 身份、RBAC 和面向人的审计记录。
- HA/failover 拓扑：PostgreSQL、Redis、MinIO 和 Orchestrator 仍按单实例思路部署。需要先确定托管服务、复制方案、故障域和成本。
- 容量目标：`deploy/ops/basic-load-soak.sh` 支持 `OJOS_LOAD_MAX_P95_MS`，但仓库没有正式吞吐、延迟和并发 SLA。当前脚本只算冒烟。
- Redis、MinIO 与 PostgreSQL 的 TLS/PKI：`OJOS_SECRET_CHECK_REQUIRE_TLS=1` 已能强制 Redis 和 MinIO 配置，证书签发、轮换和 PostgreSQL client TLS 仍需统一方案。
- 运行资产交付：Orchestrator 镜像和 alpha bundle 不携带业务服务源码、Compose 文件或业务镜像。需要决定 release 包是否带 binary/image 引用、节点如何预拉镜像，以及离线环境怎么交付。
- 跨节点凭据：node dispatch 仍把全局 `ORCHESTRATOR_INTERNAL_TOKEN` 作为控制面证明，节点被攻陷后也能拿它
  调用控制面。需要改成每节点、单向的控制面凭据，或使用签名 / mTLS。

## 可以直接实现

- 给 `PgOrchestratorStore` 加连接池和 TLS，减少每个 query/execute 都重新握手的开销。
- 降低持久化模式下的全表回读。当前写操作后会重建内存视图，数据量增加后成本明显。
- 缩小全局 console 锁范围。worker pool 已经是 32 线程加 64 项有界队列，但慢下载、外部命令和多数 core API 仍会在锁内串行。
- 加入 SIGTERM 优雅停机、Operation 恢复和 reconciler 生命周期管理。
- 给 Docker Compose、本地进程和其它外部命令设置 timeout；为 `LocalProcessDriver` 补进程监督、存活检查和 PID 复用保护。
- 实现 node-side stop/rollback 与 Service/Host 生命周期。当前只接通目标身份绑定的 install；
  `runtime_owner=node` 的升级、回滚和生命周期会明确阻塞，不能用控制面本地 driver 代替。
- 把生产 provisioner 从默认 Deferred 改成显式配置、缺失即失败，并补部署说明。
- 为 host 批量生命周期加入自动补偿。当前 Operation 会保存旧状态，也允许对 FAILED Operation 手动 rollback，但失败时不会自动执行。
- 固定 Gateway 的动态路由归属，清理 `etc/gateway.yaml` 中与 effective route table 重叠的静态配置。
- 把 API surface 的 `rate_limit`、`timeout`、`allowed_callers` 和 `denied_callers` 传播到有效路由，并在
  Gateway 里按 deny-first 规则执行；当前只记录契约。
- 启用并验证 Gateway 内部请求签名后，再开放 release API 的 `auth_mode: internal`；当前 manifest 会直接拒绝，
  避免生成不可调用的动态路由。
- 处理 release 下载的 DNS rebinding：安全校验和实际连接需要共享解析结果，或使用等价的地址固定方案。
- 扩展浏览器 E2E，覆盖控制面令牌、商店导入、Link 启停、生命周期失败、回滚和权限拒绝。
- 扩展告警与 trace 覆盖；目前只有少量合成路径。
- 把 Rust workspace 已经通过的严格 Clippy 固化为 CI 门禁；独立 judge-worker 在 Rust 1.92 下还有 18 个
  `too_many_arguments`、`collapsible_if` 等样式告警，也要先清理。另需确定 `shellcheck` 的门禁策略。

## 当前远端门禁

- [Orchestrator CI 30746067945](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30746067945) 已在
  `2a0d647` 上成功。Rust、PostgreSQL live integration、judge-worker/nsjail、Go 漏洞扫描、两个 Web
  前端和生产策略检查均通过。
- [Orchestrator Docker E2E 30746067935](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30746067935)
  已在同一 SHA 上成功。它由 push 触发，因此镜像构建、trace 和 load/soak 按 workflow 条件跳过。
- Staging Drill 最近一次成功仍是历史基线 `875586f`：
  [30717233049](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30717233049)。
- Ops Drills Nightly 最近一次已核对的运行仍是失败的
  [30718434686](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30718434686)：service 凭据与 Redis
  恢复通过，告警触发失败，Manager 冒烟被跳过。

当前还缺 `2a0d647` 的 Staging、Ops Drills，以及启用镜像、trace、load/soak 的全量 Docker E2E artifact。

## 历史边界

- Non-root Agent 的远程执行通道仍不完整。
- 创建发布 tag 前，要在干净 checkout 中重跑 release gate。
