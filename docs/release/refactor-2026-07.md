# 2026-07 编排器重构记录

本轮把原生 GUI 换成同源 Web 控制面，并补上商店、Link 启停、Service/主机生命周期和几处安全边界。这里记录当前实现，也把仍然挡住生产发布的问题写清。

## 入口与并发模型

`manager/gui` 已删除。图形入口现在位于 `manager/web`，技术栈是 Vue 3、TypeScript、Vite 和 Pinia。`FlowCanvas.vue` 自己处理平移、以指针为中心的缩放、节点拖拽、端口连线和命中判定，没有引入画布库。构建产物由 daemon 托管在 `/`，Web UI 与控制面 API 同源部署。

TUI 保留给没有浏览器的终端环境。它和 Web UI 都使用 core action 模型；Web 走 daemon REST，TUI 直接复用 Rust core。

daemon 已从单文件拆成 `http`、`auth`、`routes`、`server` 等模块。连接层使用固定工作线程池，`ORCHESTRATOR_MAX_WORKERS` 默认是 32；等待队列容量固定为 64，队列满时返回 503。响应仍使用 `Connection: close`，多数 core 请求还会取得全局 console 锁，因此线程池解决的是无界线程问题，并没有让所有业务操作并行。

## 商店与 Release

商店可以从 GitHub Release、普通 HTTP(S) URL 或本地路径导入 zip、tar.gz 和裸 `release.yaml`。`import_external_release` 会读取 release 契约，生成与之对应的 `ServiceManifest`，再把 Service 和 Release 一起写入 store。只写其中一条会让后续安装缺少必要的交叉约束。

`release.install` 继续走 Operation 的计划、确认、执行和回滚链。多版本目录会按请求版本或 Service manifest 版本选择准确的 release；同一 Service 有多个版本且无法判定时，计划阶段直接报错。

仓库内 `store/index.json` 是默认商店索引，也可用 `OJOS_STORE_INDEX_URL` 指向外部索引。目录中的 manifest 只表示可安装版本；`GET /store/index` 响应里的 `installed` 字段现在根据 `HostService` 部署记录生成，不再把整个仓库目录误报为“已安装”。

`deploy/release/pack-service-package.sh` 打出的包包含 release/service manifest 和可选 migration。它不是完整运行镜像。当前内置 release 中有些 `image` 或 `binary` 仍为空，local-process 条目还依赖仓库内 Go 源码和工作目录。只拿 Orchestrator bundle 无法启动这些服务。

## Link 和生命周期

Link 增加了持久化的 `enabled` 字段，并提供 `link.enable`、`link.disable`。停用的 Link 不参加 reconciler 健康探测，也不计入诊断报告的 unhealthy 数量。`link.update` 使用 PATCH 语义：省略的协议、鉴权、scope、启停状态、配置/密钥引用和策略都会保留，最近健康结果也不会被元数据更新清空。启停前记录原值，回滚不会机械取反。

`service.start`、`service.stop`、`service.restart` 和 `service.delete` 已接入 runtime pipeline。`host.start`、`host.stop` 会对指定 `host_ip` 上的 `HostService` 逐项执行。执行前，Operation 会保存受影响的 `HostService` 与 `DeployedServiceApi`；显式回滚按快照恢复原来的 running/stopped 状态。状态变化后会重新计算并发布 gateway 路由，已停止服务不会继续留在 effective route table。

执行运行时 driver 必须显式传 `execute_service_driver=true`。Web UI 在服务页、操作页和商店卸载动作前提供
单独的授权；`release.install` 可不勾选，只登记或延后启动，其它运行时动作未授权时不可执行。升级正在运行的固定运行时也必须授权，旧版本会在新版本启动前
停止。`release.delete` 只删除未被部署引用的历史 Release；卸载仍走 `service.delete`。driver 失败会返回失败，
不再降级成 `UNSUPPORTED`。

批量操作失败后不会自动发起补偿。快照仍在 Operation 中，operator 可以对 `FAILED` Operation 显式调用 rollback。

## API、权限和节点边界

Service 在 `release.yaml` 的 `apis` 中声明 API surface。Link 与 visibility 决定调用关系，`effective_api_routes` 解析目标 Endpoint，Gateway 按 `api_id` 转发。

权限 checker 优先调用 `{gateway}/internal/apis/{api_id}`。`auth_mode: service` 会绑定
`X-OJOS-Caller-Service`；release manifest 暂不接受 Gateway 尚未开放的 `auth_mode: internal`。auth-service
只在 `auth.user.permission.check` 的精确 permission-check 路径接受独立 service credential。这个
API ID 只能由 auth-service 声明；Gateway 还会核对实际 provider，只有两者同时匹配才转发 service bearer。
`auth_mode: service` 不能搭配 `permission: public`，其它 provider 不会收到调用方的 service token。直连
auth-service 仅保留为显式配置的回退路径。

节点安装同时使用两类凭据：节点 bearer 证明节点身份，`x-ojos-orchestrator-token` 检查控制面共享 secret，
两者不会互相覆盖。但后者目前与控制面 API 共用 `ORCHESTRATOR_INTERNAL_TOKEN`，是双向共享凭据，不能提供
控制面到节点的单向身份保证。Node 允许真实 driver 时还要打开环境上限，并配置
`ORCHESTRATOR_NODE_HOST_IP`；请求主机、Endpoint host 和节点身份不一致时会被拒绝。请求已授权 driver、但
Node 没打开执行上限时会返回 `FAILED` / `Blocked`，不会退回 metadata-only；只有未授权 driver 的请求才只登记
元数据。控制面打开 `ORCHESTRATOR_NODE_DISPATCH` 时，生产预检还要求配置 `ORCHESTRATOR_NODE_ENDPOINT`。

Gateway route publisher 强制要求明确的 gateway node ID。开启强制刷新却没有 node ID 时，发布会在发 HTTP 请求前失败，避免把空的无作用域路由表推给 Gateway。

## 下载、鉴权和 HTTP 防护

外部 release 下载只允许 HTTP(S)。每次 DNS 解析都会拒绝 loopback、私网、link-local、多播和广播地址；`ORCHESTRATOR_ALLOW_PRIVATE_RELEASE_SOURCE=1` 只放行有意使用的 loopback/私网镜像，link-local 和云元数据地址仍然禁止。重定向最多跟随 5 次，每一跳重新检查目标，GitHub token 只发送给第一跳。

解压预算是总量 512 MiB、最多 5000 个条目、单条目 64 MiB。`ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1` 后，所有 package loader 入口都要求并校验 SHA-256，不再只有 `/store` 路径受保护。

配置 `ORCHESTRATOR_INTERNAL_TOKEN` 后，除 `GET /health` 和静态文件外，控制面 API 都需要 `x-ojos-orchestrator-token`。Web UI 收到 401 后显示令牌门禁，并把令牌保存在当前浏览器的 `localStorage`。生产密钥检查要求该 token 非空且长度至少为 32。

HTTP 请求读取会在分配或切片前检查 `Content-Length` 的上限与整数溢出。超大或溢出的长度直接返回错误。

## 构建与交付

Orchestrator Dockerfile 使用三阶段构建：

1. Node 24.11 构建并 typecheck Web UI。
2. Rust 构建 daemon。
3. Debian slim 运行 daemon，使用非 root 用户并提供 `/health` 检查。

`manager/web/package-lock.json` 已纳入仓库，CI 和镜像统一使用 `npm ci`。Web package 要求 Node `^22.18.0 || >=24.11.0`，CI 与 Docker 固定到 24.11 系列。

`pack-alpha.sh` 会打包 daemon、TUI、Web dist、schema、Service/Release manifest、部署模板和商店索引。它不会打包业务服务源码、`deploy/compose/docker-compose.yml` 或业务镜像。这条边界直接影响生命周期按钮：source checkout 可以使用当前 local-process/Compose 资产；独立 bundle 必须另外提供可运行的 binary/image 和目标端配置。

## 仍未解决

| 问题 | 当前实现 | 影响 |
| --- | --- | --- |
| PostgreSQL 连接 | `PgOrchestratorStore` 每次操作新建 `NoTls` 连接，没有连接池 | 请求会产生大量握手，数据库连接数容易成为瓶颈 |
| 持久化镜像回灌 | 写操作后会重新读取多类表，重建内存视图 | 数据量增长后放大数据库开销 |
| 全局 console 锁 | 多数 core API 和下载/安装共享同一把锁 | 慢下载或外部命令会阻塞其它控制面请求 |
| 优雅停机 | 没有完整 SIGTERM 收敛；reconciler 也未接入 daemon 生命周期 | 重启期间的 Operation 缺少自动恢复 |
| LocalProcessDriver | spawn 后只保存 PID，stop 按 PID 终止 | 没有监督与自动重启，PID 复用有误杀风险 |
| 外部命令超时 | driver 的子进程调用没有统一 timeout | 卡住的 Docker/本地命令会长期占锁 |
| 节点侧回滚 | node-side install rollback 尚未实现 | 跨节点失败后可能出现控制面与节点状态不一致 |
| Provisioner | 多个 provisioner 未配置时返回 Deferred | 生产需要 fail-closed 配置和验收 |
| 批量失败补偿 | host 生命周期保存快照，但失败后不自动 rollback | operator 必须检查 FAILED Operation 并显式回滚 |
| 运行资产 | 镜像/bundle 只有控制面运行资料，不带完整业务运行资产 | 内置目录条目不能直接等同于可启动部署 |
| 数据库 TLS | PostgreSQL client 使用 `NoTls`；Compose 示例仍有 `sslmode=disable` | 数据库链路是明文 |
| DNS rebinding | 安全检查与实际连接仍可能发生二次解析 | 两次解析之间的记录替换风险仍在 |
| Gateway 静态配置 | `etc/gateway.yaml` 仍保留静态 route/trusted service 配置 | 与 Orchestrator 动态 effective routes 并存 |
| Service enable/disable | 仍为 `UNSUPPORTED` | 只有 start/stop/restart/delete 和 Link 启停进入 runtime pipeline |

## 验证记录

最终提交前工作树已通过 `cargo fmt --all -- --check`、`cargo test --workspace --all-targets`、严格
`cargo clippy --workspace --all-targets -- -D warnings`、七个 Go module 的 test/vet、两个 Web 前端在 Node 24
下的 typecheck/build/`npm audit`、全部 shell 语法、生产策略和 Manager 冒烟。独立 judge-worker 的 25 个测试
与依赖审计通过；它的严格 Clippy 在 Rust 1.92 下仍有 18 个既有样式告警。审查环境没有 `shellcheck`，Docker
daemon 也不可用，因此没有在本机运行容器级 E2E。

这些仍只是本地结果，应以最终 commit 的 GitHub Actions 为准。当前远端基线还有 Ops Drills 和 Docker E2E
失败，详见 [生产就绪证据](../production-readiness.md)。
