# 未完成事项

本目录集中记录尚未完成、需要后续设计或实现的内容。与[项目完成度总结](../completeness-summary.md)中的
"对标生产缺陷"对应，但这里聚焦"还要做什么"，并标注性质（设计决策 / 可实现改动 / 阻塞项）。

## 需要设计决策（不是小改动）

- **完整 manager 认证模型**：operator 身份 + RBAC + 审计。当前 manager GUI/TUI 以库形式 in-process 直驱
  控制平面（`manager/gui/src/main.rs`），完全无认证层。daemon 侧已加 `ORCHESTRATOR_INTERNAL_TOKEN` 门禁
  作为首付，但完整模型仍需设计。（P1，已接受风险）
- **HA / failover 拓扑**：所有数据存储（5 个 postgres、redis、minio）当前单实例单副本。需要 DB 复制
  （Patroni/托管 DB）、Redis Sentinel/Cluster、MinIO 分布式纠删、多节点编排器等拓扑与成本决策。（P2）
- **容量 SLA 数值**：`deploy/ops/basic-load-soak.sh` 已参数化并支持 `OJOS_LOAD_MAX_P95_MS`，但吞吐/延迟/
  并发的实际 SLA 目标仍需确定。当前只是冒烟，非容量包络。（P2）
- **端到端 Redis/MinIO TLS**：`secret-check.sh` 的 `OJOS_SECRET_CHECK_REQUIRE_TLS` 开关已就位，但落地需要
  PKI/证书方案（当前无 cert-gen）。（P2）

## 可实现改动（后续批次）

- **orchestrator daemon 并发化**：当前单线程阻塞 + `Connection: close`；可改为并发处理。
- **PgStore 连接池 + TLS**：当前每请求新建连接、NoTls。
- **provisioner 生产化**：七个 provisioner 默认 Deferred，需要 Configured/Http 变体的生产配置与文档。
- **node-side rollback 实现**：`dispatcher.rs` 目前显式未实现。
- **前端 E2E 扩展**：当前仅 2 条 Playwright，需覆盖 admin/权限/异常路径。
- **可观测性扩展**：增加告警规则与看板；扩展 trace 覆盖到更多路径。

## 阻塞项（等待外部/上游）

- **quick-xml `RUSTSEC-2026-0194/0195`**：经 eframe 0.31 的 Linux 桌面/Wayland 栈传递引入；兼容的 eframe
  升级当前在 Windows 上编译失败（wgpu 29 的 `windows` crate 版本分裂）。已在 Rust audit 门禁中列入白名单，
  待 eframe 新版本再处理。
- **远端 nightly / staging 首次成功 artifact**：ops-drills-nightly、staging-drill、trace/image/load
  （在 Orchestrator Docker E2E 中）需在 GitHub Actions 上取得首次成功 artifact，才能把门禁状态从
  `pending-first-run` 提升为 `passed`。需要 `gh` 或网页手动 dispatch 触发核验。

## 历史遗留（来自旧 DOCS_STATUS 的未完成边界，仅存档参考）

- Native GUI 仍需完整实现。
- Non-root Agent 远程执行通道仍需完整实现。
- Release gate 需在干净 checkout 中重新执行后再决定是否创建发布 tag。
