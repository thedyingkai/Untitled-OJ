# 项目状态总结

本文按代码、测试和远端运行记录说明当前能力，不再用主观百分比代替证据。

- 当前工作分支包含 Web 控制面、商店、生命周期和内部服务鉴权重构，尚未形成新 tag。
- 已发布的 `v0.1.0-alpha` 是历史版本，仍使用原生 GUI。
- 远端 `main` 的最新可核对提交为 `875586f`。它的 staging 演练成功，但不能证明当前未合并分支已经通过。
- 当前定位仍是 beta / 受控生产候选，不是 HA 或容量发布。

## 1. 总体

### 技术栈

- **Rust workspace**（根 `Cargo.toml`）：成员为 `services/orchestrator/core`、`services/orchestrator/backend`、
  `manager/tui`；`judge-worker` 作为独立 crate 被 `exclude`。edition 2024。关键依赖：
  `ratatui 0.30` + `crossterm 0.29`（TUI）、`clap 4`、`serde_yaml 0.9`、`ureq 3.3`（release 包与商店索引拉取）。
- **Web UI**（`manager/web`，不在 Rust workspace 内）：Vue 3 + TypeScript + Vite + Pinia，画布为零依赖自研组件；
  产物 `manager/web/dist` 由 daemon 静态托管，交付链见 [Web UI 与插件商店](orchestrator/web-ui.md)。
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

## 2. 模块状态

| 模块 | 已实现并有测试覆盖 | 主要限制 |
| --- | --- | --- |
| orchestrator core + daemon | 模型、schema 校验、Operation、PG/内存 store、Web 静态托管、32 工作线程 + 64 连接队列 | PgStore 无连接池且使用 NoTls；HTTP/1.1 每请求关闭连接；console 全局互斥 |
| 生命周期与商店 | 多版本 release 选择、包校验、Service/Host 启停、运行前态回滚、实际安装记录 | 本地/Compose 驱动需要交付物另外提供运行资产；外部资源没有通用补偿 |
| auth-service + gateway | 用户/JWT、服务凭据、权限检查、有效 API 路由、精确的 bearer 转发边界 | Gateway 内部请求签名尚未启用；权限路由切换仍需逐服务配置凭据 |
| judge-api + judge-worker | Redis Streams 队列、nsjail 判题、结果回传、恢复演练 | Redis 单实例；worker 需要 Linux、cgroup v2 和高权限容器 |
| problem/user/storage 服务 | 题目包、用户资料、local/MinIO 存储及内部 Gateway 客户端 | handler 与异常路径测试深度不一；对象存储 TLS 取决于部署 PKI |
| Web UI + TUI | 拓扑、商店、Service/Operation 生命周期、Link、日志；共用 core | TUI 进程内调用，没有独立 operator 身份；Web UI 的认证粒度只有控制面共享令牌 |
| Gateway frontend | OJ SPA、类型检查、构建和少量 Playwright E2E | E2E 数量少，管理与失败路径覆盖不足 |
| 部署与演练 | Compose、预检、备份恢复、staging、凭据、Redis、告警、trace、load 脚本 | 单机无 HA；部分远端门禁仍需在当前分支重新跑通 |

### 关键技术实现手段

- **release.install 流水线**（`core/src/store.rs`）：put service → 替换 endpoint → host-service 状态机
  `installing→dispatching→running` → upsert release record + API surface + route → migrations →
  permission 注册 + auth 注册 → frontend entry → Redis/storage 资源开通 → node dispatch → driver action →
  健康等待 → gateway route 发布。回滚由 `release.rollback` + 前态捕获实现。
- **能力状态机**：每个 action dispatch 结果标注 `REAL / RUNTIME_PIPELINE / STORE_BACKED / UNSUPPORTED / READONLY`，不受支持的
  动作绝不走假成功路径（见 [Action 模型](orchestrator/action-model.md)）。
- **权限模型**（`platform/shared/go/security/permission/permission.go`）：principal（user/team/group/service）
  + scope 递归资源边继承（CTE 深度 16）+ allow/deny 且 **deny 优先** + super_admin 短路 + 审计日志 + 过期。
- **内部服务认证**（`internalauth.go`）：HMAC-SHA256 签名、DB 背书密钥轮换、Redis nonce 防重放、时间戳偏移
  校验、body-hash 绑定。
- **判题沙箱**（`judge-worker/src/sandbox.rs`）：nsjail chroot + uid/gid 10001 + seccomp（阻断网络/内核逃逸
  syscall）+ cgroup v2 内存峰值/OOM 检测 + 只读绑定挂载白名单 + rlimit + 输出上限。
- **控制面 token 门禁**（`backend/src/auth.rs`）：未设置 `ORCHESTRATOR_INTERNAL_TOKEN` 时不认证；设置后
  除 `GET /health` 和静态文件外，所有 API 都要求 `x-ojos-orchestrator-token`。
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

### 管理入口 Web UI / TUI

```bash
# Web UI：构建 manager/web 后启动 daemon，浏览器访问 http://127.0.0.1:8090/
cargo run -p ojos-orchestrator-tui -- --repo-root .
```

设置 `ORCHESTRATOR_DATABASE_URL` 时使用持久化 store；否则用内存 store 做演示。入口分工见
[入口形态与能力边界](orchestrator/gui-tui-parity.md)，Web UI 详见 [Web UI 与插件商店](orchestrator/web-ui.md)。

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

## 4. 生产缺口

1. **单实例数据存储，无 HA**：`deploy/compose/docker-compose.yml` 每个 DB / Redis / MinIO 均单节点单副本，
   无 replica/cluster/sentinel/failover。
2. **orchestrator PgStore 无连接池、NoTls**：`core/src/database.rs` `Client::connect(..., NoTls)`，且每次
   操作新建连接（`dispatcher.rs`），无池化。
3. **daemon 仍会串行化多数 core 访问**：网络层已有工作线程池和有界队列，`GET /health` 使用启动快照，不再等待全局锁；其它 `OrchestratorActionConsole` API、下载和外部命令仍由同一个 `Mutex` 串行。响应使用 `Connection: close`，没有 keep-alive。
4. **控制面认证默认关闭**：未配置 `ORCHESTRATOR_INTERNAL_TOKEN` 时放行一切（配置后才 fail-closed）；
   metadata-only node 入口的专用 token 也保留开发期 fail-open。Node 一旦允许真实 driver，则节点 token 与控制面 token 都必须配置并同时匹配。请求已经授权 driver、但 Node 没打开执行上限时会失败，不会退回 metadata-only。
5. **operator 身份模型未完成**：TUI 进程内调用 core；Web UI 只有共享控制面令牌，没有独立身份、RBAC 或会话审计。
6. **外部 provisioner 默认 Deferred**：迁移、Redis、存储、auth 注册、包加载、路由发布和 Node dispatch
   都要显式开关与端点。结果会记录 deferred/skipped，但不会完成外部动作。
7. **运行资产不在最小交付物中**：daemon 镜像和 bundle 不含业务源码、Compose 文件或 Docker CLI。
8. **真实 driver 默认关闭**：实际启停和相应回滚都要逐次传 `execute_service_driver=true`；纯元数据 `release.install` 可以不授权。
9. **schema 与外部资源回滚不完整**：store 快照可恢复，数据库 schema、Redis、存储和 auth-service 副作用
   需要备份或服务专用补偿。
10. **Redis/MinIO 端到端 TLS 未落地**：强制策略为 opt-in；示例 Compose 仍以单机开发拓扑为主。
11. **judge-worker 需要高权限宿主**：Compose 要求 privileged/SYS_ADMIN/cgroup host/apparmor unconfined
    （nsjail 硬需求）。
12. **远端门禁不是全绿**：`875586f` 的 staging 已成功；同期 Ops Drills 在告警 webhook 失败，Docker E2E
    在 Gateway 前端依赖审计失败。本分支已修正这两个原因，但要以推送后的运行结果为准。
13. **可观测性覆盖窄**：告警和 trace 演练路径有限；load/soak 是冒烟，不是容量测试。

## 5. 未完成事项

详见 [未完成事项](unfinished/README.md)。摘要：完整 manager 认证模型、HA/failover 拓扑与容量 SLA、
Redis/MinIO 端到端 TLS、远端 nightly/staging 首次成功 artifact，以及若干代码质量项。

## 6. 使用判断

当前代码适合 beta 和受控验证环境。若用于有限生产，应至少配置独立数据库、强令牌、release checksum、网络隔离、
人工备份核对和明确的回滚手册。HA、容量、operator RBAC、连接池、端到端 TLS 与完整远端门禁收口前，不应按 GA
发布。
