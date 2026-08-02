# 耦合决策：什么留在 core，什么做成驱动

这份文档说明能力应放在业务服务、编排器 core 还是外部驱动。前四节记录当前选择，最后一节列出仍需处理的
耦合。

前置阅读：[编排器边界](../orchestrator/boundary.md) 划分编排器与 Gateway、业务服务的责任；
[入口形态与能力边界](../orchestrator/gui-tui-parity.md) 说明三个入口的分工。

## 一、服务只负责声明，其余三件事都不进服务代码

一次跨服务调用要解决四个问题。它们被刻意拆给四个不同的地方，任何一个都不允许下沉到业务服务里：

| 问题 | 由谁回答 | 载体 |
| ---- | ---- | ---- |
| 我提供什么 API | 服务自己 | `release.yaml` 的 `apis:`（`api_id` / `path_prefix` / `methods` / `visibility` / `auth_mode` / `permission`） |
| 谁被允许调我 | 编排器 | Link 授权 + `visibility` + node 树 |
| 对方在哪 | 编排器 | `service_api_surfaces` → `deployed_service_apis` → `effective_api_routes` |
| 流量怎么过去 | Gateway | `/internal/apis/{api_id}/...` 反向代理 |

服务侧只需要内部 Gateway 地址和目标 `api_id`。对端主机、端口、健康状态与调用权限由编排器计算，Gateway
负责执行。

解析链条本身在 `services/orchestrator/core/src/store.rs`：`service_api_surfaces_from_release` 把 release 声明变成
surface 行，`deployed_service_apis_from_release` 记录某个 endpoint 上真实跑起来的实例，
`effective_api_routes_from_registry` 做最后的连接——它校验 node 树无环，只保留 `status == "running"` 的部署实例，
再按 `visibility` 判定可见性（同节点要 `same-node` 或 `global`，祖先提供方要 `descendants`），最后按距离排序。
结果通过两条路径到达 gateway：gateway 主动拉 `GET /internal/orchestrator/nodes/{id}/routes`
（`services/gateway/internal/orchestrator/snapshot/client.go`），编排器在安装成功后主动推
`POST {gateway}/api/admin/orchestrator/routes/reload`（`HttpGatewayRoutePublisher`）。

### 两个实例走查

**Storage。** judge-api 的 `services/judge-api/internal/logic/storage_client.go` 与 problem-service 的
`services/problem-service/internal/storage/sync.go` 都只配 `InternalGatewayEndpoint` + `api_id`，请求打到
`{gateway}/internal/apis/storage.object.get/{bucket}/{key}`，带上 `X-OJOS-Caller-Service`、`X-OJOS-Node-Id` 与服务凭据。
storage-service 的对象路由不重复做认证；Gateway 根据有效路由中的 `auth_mode: service` 和
`permission: storage.object.read` 完成鉴权。通过后，Gateway 会删除 Authorization，不把可复用的服务凭据交给
storage-service。

**用户权限校验。** user-service、problem-service 和 judge-api 过去各自持有 `AuthService.Endpoint`，直接
`POST {auth-service}/auth/admin/permission-check`，绕过编排器与 gateway。现在
`platform/shared/go/security/permission/checker.go` 改成路由感知的 `RemoteUserChecker`：配置了
`InternalGatewayEndpoint` 就走 `{gateway}/internal/apis/auth.user.permission.check`；否则回退到
`AuthService.Endpoint`，用于单服务调试。所选路径会在构造时写入日志
（`route=internal-gateway` / `route=auth-service`），并出现在每条错误信息里。auth-service 在 `release.yaml` 里新增了
对应的 API surface：

```yaml
  - api_id: auth.user.permission.check
    path_prefix: /auth/admin/permission-check
    methods: [POST]
    visibility: descendants
    auth_mode: service
    permission: auth.permission.check
```

它与既有的 `auth.permission.check`（`/auth/permission-check`）语义不同。后者检查调用方 Service 是否持有
某个 API 权限，供 Gateway 做 `auth_mode: service` 鉴权；前者检查用户在 scope 下的权限，供业务服务调用。

## 二、刻意耦合在 core 的东西

有三块东西被有意识地放在同一个 crate（`services/orchestrator/core`）里，不做插件化。它们共享同一套不变量，拆开的
代价是状态机分裂——同一个 operation 在两个地方各有一半真相，回滚就不可能正确。

**领域模型**。`Service`（实为 `ServiceManifest`，在 `service.rs`）、`Endpoint`、`Link`、`NodeRecord`、`Operation`
都定义在 `model.rs` / `service.rs`。运行时身份统一是 `ip:port:service-name`。Link 的启用状态、Endpoint 的健康、
Node 的父子关系共同决定 `effective_api_routes` 的输出；把 Link 拆出去意味着可见性计算要跨进程读一份可能过期的副本。

**动作目录与 plan-confirm-execute-rollback 链**。`action.rs` 的 `ACTION_CATALOG` 声明每个动作的风险等级与
`ActionPlanMode`（`ReadOnly` / `Direct` / `Planned` / `ConfirmedPlan`），`planner.rs` 生成 `ActionPlanPreview`，
`model.rs` 的 `plan_operation` / `confirm_operation` / `start_operation` / `succeed_operation` / `fail_operation` /
`rollback_operation` / `cancel_operation` / `expire_operation` 是唯一的状态迁移入口，`store.rs` 的 `OperationExecutor`
负责 `apply()` 与 `rollback()`，回滚快照是类型化的结构体（`ReleaseInstallPreviousState` 等）而不是自由格式 JSON。
"哪些动作需要确认"和"确认之后执行什么"必须由同一份目录决定，否则入口之间会出现权限不一致。

**store 抽象与它的两个实现**。`OrchestratorStore` trait 在 `store.rs`，`MemoryOrchestratorStore` 与
`database.rs` 的 `PgOrchestratorStore` 是同一个 trait 的两个实现，选择在 `dispatcher.rs` 的 `ConsoleStoreMode`：
设置 `ORCHESTRATOR_DATABASE_URL` 用 PG，否则从仓库 manifest 构建内存视图。这里之所以耦合而不是做成"存储插件"，
是因为 `service_api_surfaces()` / `deployed_service_apis()` / `effective_api_routes()` 是 trait 的**默认方法**——
可见性计算的正确性属于 core，不属于某个存储后端。换存储只该换 CRUD，不该换语义。

## 三、刻意解耦为 trait / 驱动的东西

凡是"要碰外部世界"的动作，都抽成 trait，默认给一个 `Deferred*` 实现，再由 `Configured*` 包装器按环境变量决定是否
真的接线。全部定义在 `store.rs`（`ExecutionDriver` 在 `executor.rs`）。

| Trait | 默认 | 真实实现 | 环境开关 | 端点变量 |
| ---- | ---- | ---- | ---- | ---- |
| `ExecutionDriver` | 按服务 runtime 选择 | `LocalProcessDriver` / `DockerComposeDriver` / `ExternalEndpointDriver` | 控制面逐次要求 `execute_service_driver=true`；Node 端还受 `ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER` 上限约束 | `OJOS_ORCHESTRATOR_DOCKER_BINARY`、`OJOS_LOCAL_PROCESS_STATE_DIR` |
| `ReleasePackageLoader` | `DeferredReleasePackageLoader` | `LocalReleasePackageLoader` | `ORCHESTRATOR_RELEASE_PACKAGE_LOAD` | `ORCHESTRATOR_RELEASE_PACKAGE_ROOT` |
| `NodeServiceDispatcher` | `DeferredNodeServiceDispatcher` | `HttpNodeServiceDispatcher` | `ORCHESTRATOR_NODE_DISPATCH` | `ORCHESTRATOR_NODE_ENDPOINT` / `ORCHESTRATOR_NODE_TOKEN` / `ORCHESTRATOR_INTERNAL_TOKEN`；目标 Node 真执行时还要 `ORCHESTRATOR_NODE_HOST_IP` |
| `GatewayRoutePublisher` | `DeferredGatewayRoutePublisher` | `HttpGatewayRoutePublisher` | `ORCHESTRATOR_GATEWAY_ROUTE_PUBLISH` | `GATEWAY_ENDPOINT` / `GATEWAY_ADMIN_TOKEN` / `GATEWAY_NODE_ID` |
| `MigrationRunner` | `DeferredMigrationRunner` | `LocalSqlMigrationRunner` | `ORCHESTRATOR_MIGRATION_EXECUTION` | `ORCHESTRATOR_MIGRATION_ROOT`、`<SERVICE>_DATABASE_URL` |
| `RedisResourceProvisioner` | `DeferredRedisResourceProvisioner` | `TcpRedisResourceProvisioner` | `ORCHESTRATOR_REDIS_RESOURCE_SYNC` | `REDIS_ENDPOINT`（回落 `REDIS_URL`） |
| `StorageResourceProvisioner` | `DeferredStorageResourceProvisioner` | `HttpStorageResourceProvisioner` | `ORCHESTRATOR_STORAGE_RESOURCE_SYNC` | `STORAGE_SERVICE_ENDPOINT` |
| `AuthPermissionRegistrar` | `DeferredAuthPermissionRegistrar` | `HttpAuthPermissionRegistrar` | `ORCHESTRATOR_AUTH_PERMISSION_SYNC` | `AUTH_SERVICE_ENDPOINT` / `AUTH_SERVICE_ADMIN_TOKEN` |

组合根不是 `dyn`，而是 `OperationExecutor` 的九个泛型参数（store、endpoint probe，加上七个驱动位），每个驱动位的
默认类型都是对应的 `Deferred*`。生产路径在 `dispatcher.rs` 的 `dispatch_planned_action` / `dispatch_operation_apply`：
它显式传入 `ConfiguredAuthPermissionRegistrar` / `ConfiguredRedisResourceProvisioner` /
`ConfiguredStorageResourceProvisioner` / `ConfiguredMigrationRunner` 四个 `from_env()`，
`with_runtime_provisioners` 内部再把 `ReleasePackageLoader` / `GatewayRoutePublisher` / `NodeServiceDispatcher`
换成各自的 `Configured*::from_env()`。七个驱动位因此在生产路径上全部是环境驱动的。

运行时授权分两层。普通本地进程和 Compose 动作只接受本次请求里的
`execute_service_driver=true`，Operation 不会继承上一次授权。安装派发给 Node 时，Node 既要看到这次请求授权，
还要允许 `ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER`；环境变量只是目标节点的上限，不能替代用户授权。请求已经
授权、但目标 Node 没打开这个上限时，安装会返回 `FAILED` / `Blocked`，不会退回 metadata-only。只有请求本身
没有授权 driver 时，Node 才只登记元数据。
Node 接受执行后，该部署记为 `runtime_owner=node`。控制面不会再用本地 driver 启停或删除它。
目标 Node 必须同时校验专用 bearer 和控制面内部 token。若允许真实 driver，还会要求请求 `host_ip`、Endpoint
中的 host 与本机 `ORCHESTRATOR_NODE_HOST_IP` 三者一致，避免把另一台主机的安装请求落到当前节点。
控制面打开 `ORCHESTRATOR_NODE_DISPATCH` 时，生产预检还要求填写 `ORCHESTRATOR_NODE_ENDPOINT`；缺少派发地址会
直接阻止启动。

### Deferred 的含义

这是整套驱动设计里唯一重要的语义约定。`Deferred*` 实现**既不报错也不谎报成功**：它返回 `Ok`，但结果里的 `status`
是 `planned` / `deferred` / `skipped`，`reloaded: false`、`accepted: false`、`manifest_loaded: false`，并附一条说明
"某某开关未启用 / 某某端点未配置"。`DeferredMigrationRunner` 更进一步，把每条 migration 记成
`status: "registered"`、`applied_at: ""`，消息是"migration remains registered until a runner is configured"——
它明确地留下"这条迁移还没跑"的证据，而不是把未执行伪装成已执行。

`Configured*` 包装器只做两级判断：开关没开，返回 deferred 形状并说明是哪个环境变量没开；开关开了但端点缺失，同样
返回 deferred 形状并说明是哪个端点变量没配；两者都齐才委托给真实实现。真实实现遇到非 2xx 会返回
`OrchestratorError::Dependency`，失败是真失败。`DockerComposeDriver` 遵循同一约定：没有显式启用执行时，它返回
`status: "PLANNED"`，而 `ensure_driver_result_succeeded` 会把 `PLANNED` 转成 `OrchestratorError::Blocked`，
让调用方看到"被拦住了"而不是"做完了"。

这条约定的意义在于：一个只配了数据库、什么开关都没开的编排器，跑完 `release.install` 会得到一份诚实的计划
（哪些迁移待执行、哪些路由待推送、哪些包待下载），而不是一份看起来全绿、实际什么都没发生的报告。

## 四、三个入口，一套 core

| 入口 | 位置 | 与 core 的关系 |
| ---- | ---- | ---- |
| Web UI | `manager/web`（daemon 静态托管 `manager/web/dist`） | 浏览器 → daemon REST → core |
| TUI | `manager/tui`，bin `ojos-orchestrator-tui` | 进程内直接调用 `OrchestratorActionConsole` |
| daemon | `services/orchestrator/backend`，bin `ojos-orchestrator-daemon` | 进程内直接调用 core，对外只暴露 REST |

三者共用 `OrchestratorActionConsole` → `OrchestratorActionDispatcher`，提交的都是同一种 `ActionRequest`。
Web UI 独有的只是交互形态（画布坐标、商店浏览），不是能力：画布坐标走 `/ui/layout`，是纯 UI 状态，不进编排 store；
商店安装最终落到 core 的 `import_external_release` + `release.install`。

暂不增加第四种传输形态。每增加一种传输（gRPC、消息队列或 CLI 子命令树）都要重新表达
action schema、权限判定和错误映射，而这三样恰恰是最容易在副本之间漂移的东西。现有三个入口已经覆盖了全部场景：
有浏览器用 Web UI，没有浏览器用 TUI，自动化用 daemon REST。TUI 和 daemon 直接链接 core 库，因此网络协议
只有 HTTP。daemon 的模块边界还有一个源码扫描测试
（`backend/src/main.rs` 里的 `daemon_source_avoids_forbidden_boundary_terms`）在守着，防止业务逻辑漏进传输层。

## 五、仍然存在的漂移与迁移计划

本次收口后，下面这些问题**依然存在**，可以分别推进。

**1. Gateway 仍带一张写死的静态路由表。** `services/gateway/etc/gateway.yaml` 的 `Proxy.Routes` 与
`Proxy.TrustedServices` 里仍然写着 `http://auth-service:8081`、`http://problem-service:8083` 一类的固定地址。
运行时 `ServiceProxy.ServeHTTP` 的匹配顺序是：先 `/api/auth` 与 `/api/judge/worker` 这两条核心静态前缀，再
`/internal/apis/*`，再编排器下发的动态路由表，最后才轮到其余静态路由。也就是说静态表现在主要起两个作用——
兜底（编排器不可达时 gateway 仍能起来）和承载 `shouldForwardStaticAuthorization` 这两个需要透传 Authorization 的
特例。迁移方向是把 `TrustedServices` 收敛成"仅当动态路由缺失时使用的 fallback"，并把 `Routes` 里能被
`effective_api_routes` 覆盖的条目逐条删掉；`/api/auth` 因为要在用户拿到 token 之前就可用，大概率需要永久保留。

**2. 权限校验默认仍走回退路径。** 三家服务的 `AuthService.InternalGatewayEndpoint` 默认是空串，所以默认行为与改造
之前完全一致（直连 auth-service）。独立部署可以按服务设置 `OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT`；仓库 Compose
会把同名根变量显式注入三家容器，因此启用时会同时切换。这里刻意**没有**复用已有的
`OJOS_INTERNAL_GATEWAY_ENDPOINT` / `OJOS_INTERNAL_GATEWAY_URL`（storage 客户端在用），因为那两个变量在 compose
部署里已经是被设置的状态，复用会让权限校验在没有服务凭据的情况下突然切换并全线 401。

**3. 切换前需要先签发服务凭据。** 新增的 `000012_grant_service_permission_check` 迁移登记了权限
`auth.permission.check`、补齐了 `user-service` 的 service identity，并为三家调用方写入了
`service_permission_grants`（`api_id = auth.user.permission.check`）。但它**没有**种任何凭据：每个调用方需要通过
`POST /auth/admin/services/{service_code}/credentials` 拿到自己的 token，写进 `AuthService.ServiceToken`
（或 `OJOS_<SERVICE>_SERVICE_TOKEN`）之后，那个服务才能打开 gateway 路径。现存的 `000010` 迁移给
problem-service / judge-api / judge-worker 种了同一个开发用凭据哈希，生产环境应当轮换掉。代码不会再把
`AUTH_INTERNAL_TOKEN` 当作缺省 service credential；缺少独立凭据时只会使用明确配置的 auth-service 回退路径。

**4. Gateway 内部签名目前关闭。** `services/gateway/internal/svc/servicecontext.go` 中的
`internalSigner` 仍为 `nil`，提供方无法验证 Gateway 身份。Gateway 只对
`auth.user.permission.check` 这一条 `auth_mode: service` 路由转发原服务凭据，让 auth-service 再验证一次；
其他提供方一律收不到 bearer。内部签名启用后，这个单点透传也应改为验证 Gateway 签名。

**5. 回滚不能撤销所有外部副作用。** 生命周期回滚会恢复驱动状态、HostService、DeployedServiceApi，并刷新
Gateway 路由；它和应用一样要求 `execute_service_driver=true`。`release.install` 能恢复编排 store 快照，但
数据库 schema、Redis、存储和 auth-service 已完成的外部变更没有通用反向操作。相关回滚必须配合备份或服务专用
补偿步骤。

**6. 两条权限校验 surface 并存。** `auth.permission.check`（`/auth/permission-check`，服务自查）与
`auth.user.permission.check`（`/auth/admin/permission-check`，查任意用户）现在同时存在。这是有意的，因为语义不同；
但 gateway 自身在做 `auth_mode: service` 鉴权时，走的仍是 `authclient` 直连 `AuthService.Endpoint`，而不是
经由 effective route。gateway 作为控制平面的一部分自举时无法依赖自己解析出来的路由，这个自举例外需要单独论证，
目前只是既成事实。
