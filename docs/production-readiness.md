# 生产就绪证据

这是一份证据账本。只有 `headSha` 与待发布 commit 完全一致的远端运行，才能作为该版本的发布门禁。本地结果适合排错，不能替代 GitHub Actions artifact；旧 commit 的成功记录也不能证明当前改动。

## 远端基线

截至 2026-08-02，`main` 的基线 commit 仍是 `875586ff92324d8d936d71f35c24cb0f1ad494f5`。下面列的是 8 月 1 日最新一轮定时运行，只覆盖这个基线，不包含当前 Web 控制面和生命周期修复分支。

| Workflow | 结果 | 已证明的部分 | 未通过或未执行的部分 |
| --- | --- | --- | --- |
| [Staging Drill 30717233049](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30717233049) | 通过 | 备份、恢复和回滚演练完成，artifact 已上传 | 不能外推到当前分支 |
| [Ops Drills Nightly 30718434686](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30718434686) | 失败 | service 凭据生命周期、Redis 恢复通过 | 告警触发失败；Manager 冒烟被跳过 |
| [Orchestrator Docker E2E 30715126809](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30715126809) | 失败 | Rust、PostgreSQL、nsjail、Go 检查通过 | Gateway 前端的 `npm audit --audit-level=high` 仍报告 `axios` 与 `postcss` 高危问题；后续编排器模型、Compose、生产门禁、镜像、trace 和 load/soak 步骤被跳过 |

因此，当前没有可用于发布本轮重构的完整远端绿灯。

## 本地审查记录

本轮审查过程中跑过以下基线检查：

| 检查 | 结果 | 边界 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 通过 | 最终提交前工作树结果 |
| `cargo test --workspace --all-targets` | 通过 | 覆盖 Rust workspace；PostgreSQL live 测试仍依赖外部数据库 |
| `services/judge-worker` 的 `cargo test --all-targets` 与 `cargo audit` | 通过 | 25 个测试通过，独立锁文件未命中已知漏洞 |
| 七个 Go module 的 `go test ./... -count=1` | 通过 | 本地工具链结果 |
| 七个 Go module 的 `go vet ./...` | 通过 | 本地工具链结果 |
| `manager/web` 的 typecheck、build 与 `npm audit` | 通过 | Node 24.14，使用当前 `package-lock.json`；最终 commit 仍需 CI 复验 |
| Gateway frontend 的 typecheck、build 与 `npm audit` | 通过 | Node 24.14；本地已不再报告基线 run 中的 `axios`、`postcss` 高危问题 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 | 本地为 0 告警；现有 CI 还没有把 clippy 设为门禁 |
| `services/judge-worker` 的严格 Clippy | 未通过 | Rust 1.92 报告 18 个既有样式告警；当前 CI 只运行 fmt、check、test，不把它设为门禁 |
| Shell 语法与生产策略 | 通过 | 全部 `.sh` 通过 `bash -n`，`deploy/ops/ci-policy.sh` 与 Manager Web/TUI 冒烟通过；审查环境没有 `shellcheck` |
| 容器级 E2E | 未执行 | 本机 Docker daemon 不可用；Compose 渲染已由生产策略脚本检查 |

这些结果说明改动可以继续进入分支验证，不等于生产放行。

## 生产密钥策略

`deploy/ops/secret-check.sh` 支持直接变量和 `VAR_FILE`。生产配置至少满足下表要求。

| 配置 | 生产要求 |
| --- | --- |
| `JWT_SECRET` | 必填，至少 32 个字符，拒绝仓库内弱默认值 |
| `AUTH_INTERNAL_TOKEN` | 必填，至少 32 个字符 |
| `ORCHESTRATOR_INTERNAL_TOKEN` | 必填，至少 32 个字符 |
| `ORCHESTRATOR_NODE_ENDPOINT` | 启用 `ORCHESTRATOR_NODE_DISPATCH` 时必填；缺失时生产预检拒绝启动 |
| `ORCHESTRATOR_NODE_TOKEN` | 启用 node dispatch 或 Node 运行时执行上限时必填，并按节点独立配置；不能复用 JWT、内部、worker 或 service token |
| `ORCHESTRATOR_NODE_HOST_IP` | Node 允许真实 driver 时必填；请求主机和 Endpoint host 必须与它完全一致 |
| `ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM` | 必须启用，所有 release 包入口都校验 checksum |
| `OJOS_WORKER_TOKEN` | 必填，至少 32 个字符 |
| `OJOS_USER_SERVICE_TOKEN` / `OJOS_PROBLEM_SERVICE_TOKEN` / `OJOS_JUDGE_API_SERVICE_TOKEN` / `OJOS_JUDGE_WORKER_SERVICE_TOKEN` | 每个调用方使用独立凭据，均至少 32 个字符；预检会拒绝它们彼此复用或复用内部、JWT、worker token |
| 各服务 PostgreSQL 密码和 URL | 密码至少 20 个字符；URL 不得使用默认 `postgres` 用户或 localhost。其它角色的 `rolsuper` 权限需另做数据库检查 |
| `REDIS_PASSWORD` / `REDIS_URL` | 密码至少 20 个字符；URL 必须携带密码 |
| `MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD` | root user 至少 8 个字符，root password 至少 32 个字符 |
| `MINIO_ACCESS_KEY` / `MINIO_SECRET_KEY` | access key 至少 8 个字符，secret key 至少 32 个字符 |

设置 `OJOS_SECRET_CHECK_REQUIRE_TLS=1` 后，`REDIS_URL` 必须使用 `rediss://`，同时要求 `MINIO_USE_SSL=true`。

跨节点安装目前还会把 `ORCHESTRATOR_INTERNAL_TOKEN` 发给节点校验。它是控制面与节点共同持有的对称 secret：
节点一旦失陷，也可能用它反向调用控制面。因此 node dispatch 还不具备单向控制面身份保证，不能据此给不受信节点
开放生产控制面。

当前 Node 协议只接通了带双令牌和目标身份绑定的 install。`runtime_owner=node` 的升级、回滚、Service/Host
生命周期仍会明确阻塞，因为远端 stop/rollback 协议和失败恢复还没有实现。

## 放行前仍缺什么

- 在待发布 commit 上跑通 Rust、Go、Web、Gateway 浏览器 E2E、Compose 配置和生产策略检查。
- 跑通同一 commit 的 Staging Drill、Ops Drills Nightly 与 Orchestrator Docker E2E，并保留 artifact。
- 修复告警触发演练；当前最新 Ops run 因此失败。
- 复验 Gateway 前端依赖审计；基线远端 run 仍停在两个高危依赖。
- 明确运行资产交付方式。当前 Orchestrator 镜像和 alpha bundle 包含 Web 产物、schema、Service/Release manifest、模板与商店索引，不包含完整业务服务源码、Compose 文件或业务镜像。内置条目默认只能用于目录、计划和元数据注册；真正执行 local-process/container driver 需要源码 checkout，或另行提供可运行的 binary/image 和目标端运行资产。
- 完成容量、HA/failover 与端到端 TLS 决策。`basic-load-soak.sh` 是冒烟检查，不是容量证明。

在这些条件满足前，发布判定应保持 `NO-GO`。
