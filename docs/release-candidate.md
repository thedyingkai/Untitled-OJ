# 发布候选判定

## 当前结论

截至 2026-08-02，本轮重构保持 `NO-GO`。代码基线
`2a0d647ad47ccbd1b1834de95b38e55b2ef98229` 已通过常规 CI 和 push 范围的 Docker E2E，但还没有
同一 SHA 的 Staging、Ops Drills 和全量容器演练证据。

`v0.1.0-alpha` 所在的 `875586ff92324d8d936d71f35c24cb0f1ad494f5` 只保留为历史发布基线，不能替代本轮证据。

## 判定依据

| 项目 | 当前证据 | 判定 |
| --- | --- | --- |
| Rust workspace 与 judge-worker | [Orchestrator CI 30746067945](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30746067945) 通过 workspace、PostgreSQL live、judge-worker、依赖审计和严格 nsjail 测试 | 当前代码基线通过 |
| Go services | 同一 run 的七个 module test 与 `govulncheck` 通过；`go vet` 已在本地通过 | 当前代码基线通过 |
| Orchestrator Web UI | 同一 run 的 typecheck 与 build 通过；依赖审计已在本地通过 | 当前代码基线通过 |
| Gateway frontend | 同一 run 的 typecheck、build、依赖审计和 Playwright E2E 通过，artifact 已上传 | 当前代码基线通过 |
| 生产策略与 Manager 冒烟 | CI 生产策略 job 与 Docker E2E 的 production ops gates 通过；Manager 冒烟在本地通过 | 当前 SHA 仍缺 Ops Drills 中的远端 Manager 冒烟 |
| Staging Drill | [30717233049](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30717233049) 在 `875586f` 上通过 | 没有本轮 SHA 的证据 |
| Ops Drills Nightly | [30718434686](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30718434686) 在旧 SHA 上失败 | 告警演练失败，Manager 冒烟未运行；本轮未重跑 |
| Orchestrator Docker E2E | [30746067935](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30746067935) 在 `2a0d647` 上成功 | push 模式跳过镜像、trace 与 load/soak，不能替代全量演练 |
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
