# 发布候选证据

## 判定

首个生产候选 / beta 发布：CONDITIONAL GO（有条件放行）。

理由：修复 staging 演练配置和 RC 正式文档白名单后，P0 为零。核心 CI、Docker E2E、nsjail verdict 矩阵、
沙箱滥用测试、浏览器 E2E、密钥策略、本地 staging 恢复、本地可观测性演练、本地 trace E2E、本地镜像构建
和本地 basic load/soak 全部通过。发布仍为有条件，因为新修复的 nightly staging/ops/image/trace/load 门禁
在 `853423a` 之后仍需首次成功的远端 artifact。

## 候选

| 字段 | 值 |
| --- | --- |
| 已验证代码 commit | `853423a80d2ba20840867b4420a4f70da57b34af` |
| 证据 commit | 本次入库的证据更新；最终精确 hash 由 `git rev-parse HEAD` 报告 |
| 生成时间 | 2026-07-03 Asia/Shanghai |
| 建议 | CONDITIONAL GO |
| P0 数量 | 0 |
| 稳定生产范围 | beta / 首个生产候选，非完整 HA 容量发布 |

## 门禁矩阵

| 门禁 | 结果 | 类型 | 证据 |
| --- | --- | --- | --- |
| `cargo fmt --check` | 通过 | 本地 | RC 本地运行 |
| `cargo test --workspace` | 通过 | 本地 | 31 daemon、12 GUI、9 TUI、176 core、pg 集成全部通过（当时把 RC 文档加入正式文档白名单后）|
| Go 测试：auth-service | 通过 | 本地 | `go test ./...` |
| Go 测试：gateway | 通过 | 本地 | `go test ./...` |
| Go 测试：storage-service | 通过 | 本地 | `go test ./...` |
| Go 测试：judge-api | 通过 | 本地 | `go test ./...` |
| Go 测试：problem-service | 通过 | 本地 | `go test ./...` |
| Go 测试：user-service | 通过 | 本地 | `go test ./...` |
| judge-worker cargo test | 通过 | 本地 | 25 个测试，含 nsjail 矩阵和沙箱滥用测试 |
| Docker Compose config | 通过 | 本地 | `docker compose -f deploy/compose/docker-compose.yml config --quiet` |
| gateway 前端构建 | 通过 | 本地 | `npm run build` |
| gateway 浏览器 E2E | 通过 | 本地 / ci | 本地 `npm run test:e2e`；CI `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| `git diff --check` | 通过 | 本地 | RC 本地运行 |
| Redis live 集成 | 通过 | ci | `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| MinIO live 集成 | 通过 | ci | `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` |
| Orchestrator Docker E2E | 通过 | ci | `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416062` |
| nsjail verdict 矩阵 | 通过 | ci / 本地 | CI 与本地 judge-worker 测试 |
| 沙箱滥用测试 | 通过 | ci / 本地 | CI 与本地 judge-worker 测试 |
| 生产密钥 fail-fast | 通过 | ci / 本地 | CI 与本地 `deploy/ops/ci-policy.sh` |
| 备份 -> 恢复 -> 回滚演练 | pending-first-run；本地通过 | nightly / 本地 | `artifacts/rc-staging-drill-2/manifest.json` |
| service 凭据生命周期 | pending-first-run；本地通过 | nightly / 本地 | `artifacts/rc-service-credential-drill/manifest.json` |
| Redis 恢复演练 | pending-first-run；本地通过 | nightly / 本地 | `artifacts/rc-redis-recovery-drill/manifest.json` |
| MinIO 恢复演练 | pending-first-run；本地通过 | nightly / 本地 | `artifacts/rc-staging-drill-2/manifest.json` |
| 告警触发演练 | pending-first-run；本地通过 | nightly / 本地 | `artifacts/rc-alert-firing-drill/manifest.json` |
| trace E2E | pending-first-run；本地通过 | docker-e2e scheduled / 本地 | `artifacts/rc-trace-e2e-drill/manifest.json` |
| 镜像构建 | pending-first-run；本地通过 | docker-e2e scheduled / 本地 | `artifacts/rc-image-build/manifest.json` |
| basic load/soak 冒烟 | pending-first-run；本地通过 | docker-e2e scheduled / 本地 | `artifacts/rc-basic-load-soak/manifest.json` |

## 模块就绪度

| 模块 | 就绪度 | 说明 |
| --- | ---: | --- |
| orchestrator-core/backend | 91% | CI、pg 集成、release install/rollback 模型、注册表路由检查通过 |
| judge-worker | 92% | nsjail 矩阵与滥用测试通过；非形式化沙箱证明 |
| judge-api | 89% | Redis 任务队列、trace 传播、worker 结果路径已覆盖 |
| auth-service | 88% | 权限播种与凭据生命周期证据齐备 |
| gateway backend | 87% | proxy/auth/route 检查通过 |
| gateway frontend | 86% | 存在浏览器 E2E 但很少 |
| problem-service | 85% | 包校验与存储集成已覆盖 |
| storage-service | 88% | 本地/MinIO 路径与 tracing 已覆盖 |
| user-service | 84% | 基本服务测试通过；生产面较小 |
| platform/shared | 86% | 复用日志/tracing/中间件 |
| manager GUI | 80% | 仅 operator 冒烟；auth deferred |
| manager TUI | 80% | 仅 operator 冒烟；auth deferred |
| deploy/ops | 86% | 本地演练通过；远端 nightly 首次成功待定 |
| PostgreSQL | 90% | live 集成与备份/恢复证据 |
| Redis | 87% | live 集成与本地恢复演练证据 |
| MinIO | 87% | live 集成与本地恢复/回读证据 |
| Jaeger/可观测性 | 84% | 本地告警与 trace 演练通过；覆盖仍窄 |
| sdk/sets/docs | 83% | 发布文档/清单齐备；operator 打磨仍在进行 |

工程成熟度：90%。
稳定生产就绪度：85%。

工程成熟度衡量代码结构、测试、契约和运维工具。稳定生产就绪度在成熟度基础上，对未验证的远端演练、
HA/容量缺口和已接受的运维风险进行折减。

## P0/P1 状态

| 项 | 级别 | 状态 | 修复 / 风险 |
| --- | --- | --- | --- |
| main CI red | P0 | 已清除 | CI 在 `https://github.com/thedyingkai/Untitled-OJ/actions/runs/28623416077` 通过 |
| compose 生产 profile 无法 config | P0 | 已清除 | 本地 compose config 通过 |
| 密钥 fail-fast 弱默认 | P0 | 已清除 | `deploy/ops/ci-policy.sh` 通过 |
| judge-worker verdict 矩阵 / 沙箱滥用 | P0 | 已清除 | CI 与本地测试通过 |
| RC 文档使 `cargo test --workspace` 违反正式文档白名单 | P0 | 已修复 | 正式文档白名单已为 RC 证据文档更新 |
| staging 演练 storage-service 配置缺 Jaeger | P1 | 已修复 | `853423a` |
| 当前 nightly 首次成功 artifact 待定 | P1 | 已接受风险 | 本地当前 RC 演练通过；GA 前等待 scheduled artifact |
| manager auth deferred | P1 | 已接受风险 | 仅 beta 只读/dev-ops 模式 |
| alert/trace 覆盖窄 | P1 | 已接受风险 | 仅一条触发规则和一条判题 trace 路径 |
| schema rollback 不支持 | P1 | 已接受风险 | 仅应用层回滚 |
| load/soak 是短冒烟 | P1 | 已接受风险 | 非容量证据；已加入可选 p95 上限（`OJOS_LOAD_MAX_P95_MS`）|
| orchestrator daemon 控制面无鉴权 | P1 | 已加固 | 现对 internal + 变更路由强制 internal token（未设时 fail-open）；见「RC 后 beta 加固」|
| MinIO storage-service 使用 root 凭据 | P1 | 已加固 | 通过 `minio-init` 提供 scoped 最小权限用户 + bucket 策略 + 生命周期；见「RC 后 beta 加固」|

## 已接受风险

- nightly staging/ops/image/trace/load artifact 在 `853423a` 之后待首次成功；本地 RC 证据可用。
- Manager GUI/TUI 为只读/dev-ops beta，auth deferred。
- Schema 回滚不支持；release 回滚是应用层的。
- 告警触发仅覆盖一条合成规则。
- Trace E2E 覆盖判题提交路径，Redis 边界由元数据/link 语义表示。
- Load/soak 是短冒烟，非容量规划。
- 本 beta 不声称任何 HA/failover 拓扑。

## 延期项

- P2：把浏览器 E2E 覆盖扩展到最小 login/problem/submission/result 路径之外。
- P2：增加更多可观测性规则与看板。
- P2：正式 HA 部署模式与 failover 演练。
- P2：更长的 load/soak 与容量包络（基础机制已通过 `OJOS_LOAD_MAX_P95_MS` 提供；SLA 数值仍待决策）。
- P1（已接受风险）：完整 manager auth 模型 —— operator 身份、RBAC 和审计 —— 超出 daemon internal-token 门禁；更丰富的 operator 流程。
- P2：端到端 Redis/MinIO TLS —— 需要 PKI/证书决策。强制已接入 `OJOS_SECRET_CHECK_REQUIRE_TLS`（默认关），待证书。
- P3：quick-xml `RUSTSEC-2026-0194/0195` —— 经 eframe 0.31 的 Linux 桌面/Wayland 栈传递引入；兼容的 eframe 升级当前在 Windows 上编译失败（wgpu 29 的 `windows` crate 版本分裂）。已在 Rust audit 门禁中列入白名单；待 eframe 新版本再处理。

## RC 后 beta 加固

以下内容在 `853423a` RC 快照之后落地，已本地验证。远端首次成功 artifact 仍记为待定（见「已接受风险」）。

- Orchestrator daemon 控制面现对所有变更路由，以及 `internal/*` snapshot/route 读取和 per-node 有效路由表，
  强制 `ORCHESTRATOR_INTERNAL_TOKEN`（gateway 以 `x-ojos-orchestrator-token` 发送）。未设 token 时 fail-open
  （dev 与 ops 演练），设置后 fail-closed。`GET /health` 保持开放。已单元测试。
- MinIO：storage-service 不再使用 root 账户。一次性 `minio-init` 服务创建 bucket、一个仅限这些 bucket 与
  storage-service 所用对象动词的 scoped 策略、scoped service 用户，以及 artifact bucket 上 30 天生命周期过期。
  已对真实 MinIO 端到端验证：scoped 用户能读/写/删配置内 bucket 对象，但被拒绝建桶、写未列桶和 admin 动作。
- `secret-check.sh` 新增 `OJOS_SECRET_CHECK_REQUIRE_TLS=1`（默认关），在存在 TLS 端点后要求 `rediss://` 与
  `MINIO_USE_SSL=true`。
- `basic-load-soak.sh` 新增可选 `OJOS_LOAD_MAX_P95_MS` 延迟上限，记录在 `metrics.json`，仅设置时强制。
