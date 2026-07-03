# 生产就绪证据

本矩阵区分已证明的门禁、本地演练证据，以及仍需首次远端运行的新配置演练。

发布候选冻结的已验证代码 commit：`853423a80d2ba20840867b4420a4f70da57b34af`。

## 当前证据

| 能力 | 门禁 | 状态 | 证据 |
| --- | --- | --- | --- |
| Redis live 集成 | ci | 通过 | Orchestrator CI：`https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077`。 |
| MinIO live 集成 | ci | 通过 | Orchestrator CI：`https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077`。 |
| Docker E2E | ci | 通过 | Orchestrator Docker E2E：`https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416062`。 |
| nsjail verdict 矩阵 | ci | 通过 | 严格的 judge-worker nsjail live 测试要求真实 nsjail。 |
| 沙箱加固 | ci | 通过 | seccomp 策略、mount 白名单、cgroup 策略、runtime lock 和 live nsjail 测试。 |
| staging 备份/恢复/回滚 | nightly | pending-first-run；本地通过 | `deploy/ops/staging-drill.sh`、`Staging Drill` workflow。当前 RC 本地真实恢复已验证：`artifacts/rc-staging-drill-2/manifest.json`。 |
| gateway 浏览器 E2E | ci | 通过 | 带 trace/截图/视频产物的 Playwright 测试；本地与 Orchestrator CI 通过。 |
| manager GUI/TUI operator 冒烟 | nightly | pending-first-run；本地通过 | `deploy/ops/manager-smoke.sh` 记录 `manager_auth=deferred` 和只读/dev-ops beta 模式。当前 RC 本地证据：`artifacts/rc-manager-smoke/manifest.json`。 |
| 告警触发 | nightly | pending-first-run；本地通过 | Prometheus + Alertmanager webhook 演练。当前 RC 本地证据：`artifacts/rc-alert-firing-drill/manifest.json`。 |
| trace E2E | docker-e2e scheduled | pending-first-run；本地通过 | `deploy/ops/trace-e2e-drill.sh` 向 Jaeger 查询 gateway-service、judge-api-service、storage-service 和 judge-worker。在 scheduled/dispatched 的 `Orchestrator Docker E2E` workflow 中运行。当前 RC 本地证据：`artifacts/rc-trace-e2e-drill/manifest.json`。 |
| 密钥策略 | ci | 通过 | 增加了 Redis 密码和 `.env.production.example` 生产 fail-fast 策略；本地 `deploy/ops/ci-policy.sh` 和 Orchestrator CI 通过。可选 TLS 强制通过 `OJOS_SECRET_CHECK_REQUIRE_TLS=1` 提供。 |
| 镜像构建证据 | docker-e2e scheduled | pending-first-run；本地通过 | Scheduled Docker 构建在 `Orchestrator Docker E2E` workflow 中上传镜像证据。当前 RC 本地镜像构建证据：`artifacts/rc-image-build/manifest.json`。 |
| service 凭据生命周期 | nightly | pending-first-run；本地通过 | allow/deny/revoke/expire 矩阵当前 RC 本地证据：`artifacts/rc-service-credential-drill/manifest.json`。 |
| Redis 恢复 | nightly | pending-first-run；本地通过 | pending/claim/AOF 重启和 judge-api 队列状态 API 当前 RC 本地证据：`artifacts/rc-redis-recovery-drill/manifest.json`。 |
| MinIO 样本恢复 | nightly | pending-first-run；本地通过 | 由 staging 演练 MinIO 对象恢复加 storage-service 回读覆盖：`artifacts/rc-staging-drill-2/manifest.json`。 |
| load/soak | docker-e2e scheduled | pending-first-run；本地通过 | `deploy/ops/basic-load-soak.sh` 覆盖 auth 登录、题目列表、存储 put/get、判题提交和结果查询；可选 p95 上限通过 `OJOS_LOAD_MAX_P95_MS`。在 `Orchestrator Docker E2E` workflow 中运行。当前 RC 本地证据：`artifacts/rc-basic-load-soak/manifest.json`。 |

## 密钥生命周期

| 密钥 | Dev 默认 | 生产策略 | 轮换 |
| --- | --- | --- | --- |
| `JWT_SECRET` | `.env.example` 中为空 | 必需，至少 32，拒绝弱值 | 由 env/密钥管理器重启支持 |
| `AUTH_INTERNAL_TOKEN` | `.env.example` 中为空 | 必需，至少 32 | 由 env/密钥管理器重启支持 |
| `ORCHESTRATOR_INTERNAL_TOKEN` | `.env.example` 中为空 | 必需，至少 32 | 由 env/密钥管理器重启支持 |
| `OJOS_WORKER_TOKEN` | `.env.example` 中为空 | 必需，至少 32 | 由 env/密钥管理器重启支持 |
| DB 密码 | `.env.example` 中为空 | 每个服务 DB 密码必需，DB URL 不得用超级用户 | 由 DB 凭据轮换加服务重启支持 |
| `REDIS_PASSWORD` | `DEV_ONLY_redis_password_not_for_production` | 必需，至少 20，Redis URL 必须含密码 | 由 Redis/服务重启支持 |
| `MINIO_ROOT_PASSWORD` | `.env.example` 中为空 | 必需，至少 32 | 由 MinIO 凭据轮换支持 |
| `MINIO_ACCESS_KEY` / `MINIO_SECRET_KEY` | `.env.example` 中为空 | 必需，access 至少 8，secret 至少 32 | 由 MinIO 凭据轮换支持 |

## 剩余证据缺口

- 修复 staging 演练 storage-service 配置和 RC 证据文档的正式文档白名单后，当前 RC 的 P0 数量为零。
- 新增的 nightly 演练有当前 RC 本地通过证据，但在 `853423a` 之后仍需首次成功的 GitHub Actions artifact，
  其门禁状态才能从 `pending-first-run` 提升为 `passed`。
- Trace E2E 当前证明了一次通过 Jaeger 的真实本地 compose 提交，带 Redis 元数据边界和一个 judge-worker 原生
  OTLP consumer span；它仍需首个 scheduled 的 `Orchestrator Docker E2E` artifact。
- Basic load/soak 只是冒烟测试，不是容量测试，仍需首个 scheduled 的 `Orchestrator Docker E2E` artifact。
