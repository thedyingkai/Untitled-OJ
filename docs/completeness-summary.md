# 项目完成度总结

本文件给出 OJOS Orchestrator 平台的整体完成度、逐模块状态、功能用法，以及对标真实生产部署的缺陷。
所有结论尽量绑定代码位置（`文件:行`）与已有证据文档，完成度百分比给出区间并注明依据，不虚高。

- 已验证代码基线：`main`（当前 HEAD）。
- 现有自评（见 [发布候选证据](release-candidate.md)）：判定 **CONDITIONAL GO**，工程成熟度 90%，
  稳定生产就绪度 85%。
- 适用范围：**beta / 首个生产候选**，不是完整 HA 容量发布。

## 1. 总体

### 技术栈

- **Rust workspace**（根 `Cargo.toml`）：成员为 `services/orchestrator/core`、`services/orchestrator/backend`、
  `manager/gui`、`manager/tui`；`judge-worker` 作为独立 crate 被 `exclude`。edition 2024。关键依赖：
  `eframe 0.31`（GUI）、`ratatui 0.30` + `crossterm 0.29`（TUI）、`clap 4`、`serde_yaml 0.9`。
- **Go 服务**（go-zero/goctl 脚手架，各 `go.mod` 均 `replace ojos-shared => ../../platform/shared/go`）：
  auth-service、gateway、judge-api、problem-service、storage-service、user-service。
- **judge-worker**：独立 Rust crate（tokio、reqwest、sha2、nsjail）。
- **前端**（`services/gateway/frontend`）：Vue 3.5 + Vite + naive-ui + Pinia + vue-router + TypeScript；
  E2E 用 Playwright。
- **基础设施镜像**（`deploy/compose/docker-compose.yml`）：postgres:17（每服务一实例）、redis:8.8.0、
  minio、jaeger:2.19.0。

### 架构一句话

OJOS Orchestrator 是**服务编排器**（控制平面），负责导入、校验、规划、安装、连接、启停、观测和诊断
Service；OJ 业务（题目、提交、判题、用户等）由各具体 Service 实现，编排器不碰业务逻辑。核心对象为
Service / Set / Endpoint / Link / Operation / Topology / LogView / DiagnosticReport，Endpoint 身份固定为
`ip:port:service-name`，不存在 `instance-id`。

## 2. 逐模块完成度

完成度区间依据：RC 自评 + 实现深度 + 测试类型 + 已知生产缺陷综合加权。

| 模块 | 做什么 | 关键文件 | 完成度 | 测试类型 | 主要生产缺陷 |
| --- | --- | --- | ---: | --- | --- |
| orchestrator-core + daemon | 控制平面：模型/校验/计划/执行/回滚/诊断 | `core/src/store.rs`、`database.rs`、`dispatcher.rs`、`backend/src/main.rs` | 88–91% | 单元 + pg live 集成 + daemon 单元 | daemon 单线程阻塞、PgStore NoTls 无池化、7 个 provisioner 默认 Deferred、node-side rollback 未实现 |
| judge-worker | nsjail 沙箱内编译+运行+判题 | `worker_link.rs`、`sandbox.rs`、`judge.rs`、`cgroup.rs` | 88–92% | 25 测试含 nsjail live 矩阵 + 滥用 | 需 privileged 宿主；非形式化沙箱证明 |
| judge-api | 提交流程、Redis 队列、结果查询 | `logic/createsubmissionlogic.go`、`logic/queue_signal.go` | 87–89% | 单元 + Redis live 冒烟 | 依赖 Redis Streams 单实例 |
| auth-service | 登录/注册、RBAC、service 凭据生命周期 | `handler/`、`platform/shared/go/security/permission/permission.go` | 85–88% | 单元 + 权限冒烟 + 凭据 nightly | 凭据生命周期 nightly 首次成功待定 |
| gateway backend | 反向代理 + 认证网关 + 路由表消费 + admin | `internal/proxy/proxy.go`、`orchestrator/snapshot/client.go` | 84–87% | 单元 + 浏览器 E2E | 路由表依赖 orchestrator snapshot |
| gateway frontend | OJ 业务 SPA | `services/gateway/frontend/src/` | 77–86% | Playwright（2 条，很少）| E2E 覆盖窄，admin/异常路径回归风险 |
| storage-service | local + MinIO 双后端对象存储 | `internal/store/object_store.go`、`minio_store.go` | 82–88% | 单元 + MinIO live 集成 | 默认 local 后端；端到端 TLS 待 PKI |
| problem-service | 题目/用例/题目包校验 | `internal/packagefs/packagefs.go` | 82–85% | 单元含 path traversal | handler/repository 覆盖较弱 |
| user-service | 用户资料/偏好/统计 | `internal/handler/`、`store/profile_store.go` | 74–84% | 单元 + 权限 | 生产面较小，多为薄逻辑 |
| manager GUI/TUI | 运维工具，直驱控制平面 | `manager/gui/src/main.rs`、`manager/tui/src/main.rs` | 70–80% | GUI 12 / TUI 9 单元 | **无认证**（in-process 直驱 core 库），仅 beta 只读/dev-ops |
| platform/shared | 共享 Go 库 + orchestrator schemas | `platform/shared/go/security/*`、`platform/schemas/orchestrator/*` | 82–86% | 单元 | 无依赖漏洞门禁（已在 CI 增补 govulncheck/cargo-audit）|
| sdk/templates/sets | service manifest 模板；sets 已降级 | `sdk/service-sdk/`、`services/*/service.yaml` | 80–83% | 契约测试 | 无代码级 SDK；sets 概念降级为派生查询（daemon 返回 HTTP 410）|
| deploy/compose+ops | 单机全栈 + 演练工具 | `deploy/compose/docker-compose.yml`、`deploy/ops/*.sh` | 68–86% | ci-policy + nightly 演练 | 单机无 HA；多个 nightly 门禁 pending-first-run |

### 关键技术实现手段

- **release.install 流水线**（`core/src/store.rs`）：put service → 替换 endpoint → host-service 状态机
  `installing→dispatching→running` → upsert release record + API surface + route → migrations →
  permission 注册 + auth 注册 → frontend entry → Redis/storage 资源开通 → node dispatch → driver action →
  健康等待 → gateway route 发布。回滚由 `release.rollback` + 前态捕获实现。
- **能力状态机**：每个 action dispatch 结果标注 `REAL / STORE_BACKED / UNSUPPORTED / READONLY`，不受支持的
  动作绝不走假成功路径（见 [Action 模型](orchestrator/action-model.md)）。
- **权限模型**（`platform/shared/go/security/permission/permission.go`）：principal（user/team/group/service）
  + scope 递归资源边继承（CTE 深度 16）+ allow/deny 且 **deny 优先** + super_admin 短路 + 审计日志 + 过期。
- **内部服务认证**（`internalauth.go`）：HMAC-SHA256 签名、DB 背书密钥轮换、Redis nonce 防重放、时间戳偏移
  校验、body-hash 绑定。
- **判题沙箱**（`judge-worker/src/sandbox.rs`）：nsjail chroot + uid/gid 10001 + seccomp（阻断网络/内核逃逸
  syscall）+ cgroup v2 内存峰值/OOM 检测 + 只读绑定挂载白名单 + rlimit + 输出上限。
- **控制面 token 门禁**（`backend/src/main.rs`）：`ORCHESTRATOR_INTERNAL_TOKEN` 未设时 fail-open，设置后对
  变更路由与 `internal/*` 读取 fail-closed，`GET /health` 恒开。
- **MinIO 最小权限**（`deploy/compose/minio-init.sh`）：一次性 init 创建 bucket + scoped 用户 + bucket 策略
  + 30 天生命周期，storage-service 以 scoped 用户而非 root 连接。

## 3. 功能怎么用

### 起本地全栈

```bash
docker compose -f deploy/compose/docker-compose.yml up -d
# 健康检查
curl -fsS http://127.0.0.1:8090/health   # orchestrator daemon
curl -fsS http://127.0.0.1:8080/health   # gateway
```

### 编排器 daemon（HTTP 控制面）

REST 入口见 [Action 模型 · 后端 API](orchestrator/action-model.md)，如
`POST /actions`、`POST /endpoints`、`POST /operations/plan`、`POST /releases/{name}/install`、
`GET /topology`、`GET /operations/{id}/logs`。生产环境需设置 `ORCHESTRATOR_INTERNAL_TOKEN`，请求带
`x-ojos-orchestrator-token` 头。

### 管理入口 GUI / TUI

```bash
cargo run -p ojos-orchestrator-gui -- --repo-root .
cargo run -p ojos-orchestrator-tui -- --repo-root .
```

设置 `ORCHESTRATOR_DATABASE_URL` 时使用持久化 store；否则用内存 store 做演示。GUI/TUI 能力等价，见
[GUI / TUI 等价性](orchestrator/gui-tui-parity.md)。

### 判题

支持语言：cpp17、cpp20、c11、python3、java17（`judge-worker/config/languages.yaml`；compose 默认
`OJOS_SUPPORTED_LANGUAGES` 省略 cpp20）。判定结果：`ACCEPTED / WRONG_ANSWER / COMPILE_ERROR /
RUNTIME_ERROR / TIME_LIMIT_EXCEEDED`，加沙箱层 `MemoryLimitExceeded / OutputLimitExceeded`。runner 模式
只支持 `nsjail`（无 fake/dev runner）。

### 运维脚本（`deploy/ops/`）

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh          # 生产预检
OJOS_ENV_FILE=... deploy/ops/secret-check.sh                            # 密钥策略（OJOS_SECRET_CHECK_REQUIRE_TLS=1 可选强制 TLS）
deploy/ops/basic-load-soak.sh                                          # load/soak 冒烟（OJOS_LOAD_MAX_P95_MS 可选 p95 上限）
deploy/ops/staging-drill.sh                                            # 备份/恢复/回滚演练
```

完整部署与排障见 [部署清单](ops/deployment-checklist.md) 与 [运维手册](ops/ops-runbook.md)。

## 4. 对标实际生产部署的缺陷

以下缺陷均可从代码/配置直接观察，是当前 beta 与"完整生产"之间的差距：

1. **单实例数据存储，无 HA**：`deploy/compose/docker-compose.yml` 每个 DB / Redis / MinIO 均单节点单副本，
   无 replica/cluster/sentinel/failover。
2. **orchestrator PgStore 无连接池、NoTls**：`core/src/database.rs` `Client::connect(..., NoTls)`，且每次
   操作新建连接（`dispatcher.rs`），无池化。
3. **daemon 单线程阻塞**：`backend/src/main.rs` 顺序处理连接 + `Connection: close`，无并发/无 keep-alive。
4. **控制面认证 fail-open**：未配置 `ORCHESTRATOR_INTERNAL_TOKEN` 时放行一切（仅配置后 fail-closed）；
   node-token 同样未配置即放行。
5. **manager 无认证**：GUI/TUI in-process 直驱控制平面库，无 operator 身份/RBAC/审计。
6. **provisioner 默认 Deferred（no-op）**：`core/src/store.rs` 七个能力默认返回 "not configured"；生产必须
   显式配置 env 启用，否则迁移/资源开通/route 发布静默跳过。
7. **node-side rollback 未实现**：`dispatcher.rs` 明确 "node-side service install rollback is not implemented"。
8. **真实 driver 执行默认关闭**：仅当 action 带 `execute_service_driver=true` 才真正启动进程。
9. **schema rollback 不支持**：仅应用层回滚。
10. **Redis/MinIO 端到端 TLS 未落地**：强制为 opt-in 且默认关；compose Redis 明文 + 弱默认密码。
11. **judge-worker 需 privileged 宿主**：compose 要求 privileged/SYS_ADMIN/cgroup host/apparmor unconfined
    （nsjail 硬需求）。
12. **nightly drills 均 pending-first-run**：staging/credential/redis-recovery/alert/trace/image/load 尚无
    远端 CI 首次成功 artifact。
13. **可观测性覆盖窄**：仅一条告警规则和一条判题 trace 路径；load/soak 仅冒烟非容量测试。

## 5. 未完成事项

详见 [未完成事项](unfinished/README.md)。摘要：完整 manager 认证模型、HA/failover 拓扑与容量 SLA、
Redis/MinIO 端到端 TLS、远端 nightly/staging 首次成功 artifact，以及若干代码质量项。

## 6. 结论

当前项目在正确性、核心架构和主 CI 上较扎实（HEAD 的 CI、Docker E2E、真实 Redis/MinIO、强制 nsjail 均通过），
适合 **beta** 与**受控的 limited production**（受控流量 + 人工备份检查 + 网络隔离 + 明确 accepted risk），
**不适合 GA**——nightly/staging 首次成功 artifact、HA、容量和部分关键安全策略尚未闭环。
