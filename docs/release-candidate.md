# 发布候选判定

## 当前结论

截至 2026-08-02，本轮重构保持 `NO-GO`。包含当前改动的 commit 尚未通过全部发布门禁。

`main` 当前基线是 `875586ff92324d8d936d71f35c24cb0f1ad494f5`。它不包含待审查的 Web 控制面、商店、Link 启停、生命周期回滚和权限链修复，因此不能作为本轮候选 SHA。旧文档记录的 `853423a80d2ba20840867b4420a4f70da57b34af` 也只保留历史意义。

## 判定依据

| 项目 | 当前证据 | 判定 |
| --- | --- | --- |
| Rust workspace | 审查期间本地 `cargo fmt` 与 `cargo test --workspace --all-targets` 通过 | 可进入 CI，不能替代候选 commit 的远端结果 |
| judge-worker | 独立 crate 的 25 个测试与依赖审计通过；严格 Clippy 仍有 18 个既有样式告警 | 现有 CI 门禁可执行，代码质量债务仍需单独处理 |
| Go services | 七个 module 的 `go test ./... -count=1` 与 `go vet ./...` 本地通过 | 可进入 CI |
| Orchestrator Web UI | typecheck、build 和当前 lockfile 的 `npm audit` 本地通过 | 需要候选 commit 的 CI 复验 |
| Gateway frontend | Node 24 下 typecheck、build 和 `npm audit` 本地通过，0 个已知漏洞 | 基线失败原因已在本地消除，仍要由候选 commit 的 CI 证明 |
| 生产策略与 Manager 冒烟 | `ci-policy.sh`、全部 shell 语法和 Manager Web/TUI 冒烟本地通过 | 容器级路径仍需远端 Docker E2E |
| Staging Drill | [30717233049](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30717233049) 在基线 commit 上通过 | 只证明基线 |
| Ops Drills Nightly | [30718434686](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30718434686) 失败 | 告警演练失败，Manager 冒烟未运行 |
| Orchestrator Docker E2E | [30715126809](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30715126809) 失败 | Gateway 前端依赖审计失败，后续门禁被跳过 |
| 运行资产 | Orchestrator 镜像和 bundle 不携带完整业务服务源码、Compose 文件或业务镜像 | local-process/container 生命周期不能仅靠当前 bundle 完成 |
| 容量与 HA | 只有短时 load/soak 冒烟；没有正式 HA/failover 证据 | 不满足稳定生产发布 |

完整证据和密钥要求见 [生产就绪证据](production-readiness.md)。

## 晋级条件

发布候选至少满足以下条件：

1. 记录唯一候选 commit，并冻结源码、lockfile、schema 和文档。
2. 在该 commit 上跑通 Rust、Go、Web、Gateway 浏览器 E2E、Compose 和生产策略检查。
3. 同一 commit 的 Staging Drill、Ops Drills Nightly 与 Orchestrator Docker E2E 全部成功，artifact 可下载。
4. 告警触发、Manager Web/TUI 冒烟、镜像构建、trace 和 basic load/soak 都实际执行，不能因前序失败被跳过。
5. 生产环境启用 `ORCHESTRATOR_INTERNAL_TOKEN`、`ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1`，并通过 `deploy/ops/preflight.sh`。
6. 写清运行资产来源。若使用 local-process 或 container driver，目标节点必须拿到对应 binary、image、Compose 或源码目录。

## 即使晋级也不代表什么

首个 beta 不声明 HA、自动 failover、容量 SLA 或 schema 级回滚。`LocalProcessDriver` 也不是生产级进程监督器。上述边界必须继续保留在发布说明中，不能用一个绿色 workflow 概括过去。
