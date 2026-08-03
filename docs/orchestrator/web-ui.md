# Orchestrator v1 Web UI

Web UI 是 Desktop WebView 与远程 daemon 共用的图形控制面。Desktop 启动内嵌 backend、随机 loopback 端口和 loopback Agent，再由原生 WebView 加载同源页面；默认路径不会打开外部浏览器。远程模式由 daemon 直接托管同一份 `manager/web/dist`。

前端只使用 `/api/v1` 正式契约。旧的 `/store/install`、`/ui/layout`、Node push/bearer 和通用 CRUD 不属于 v1 页面能力。

## 构建与验证

Node.js 需满足 `^22.18.0 || >=24.11.0`；CI、Docker 和 release 使用 24.11。

```bash
cd manager/web
npm ci
npm run typecheck
npm test
npm run build
npm run test:e2e
```

构建产物位于 `manager/web/dist`。开发模式可运行 `npm run dev`；Vite 监听 `127.0.0.1:5174`，并把控制面请求代理到 `127.0.0.1:8090`。正式 Desktop 从 bundle 内定位 Web 产物；生产 daemon 缺少 `index.html` 时在绑定端口前失败。

## 页面与能力

页面是否显示按钮由 `/api/v1/capabilities` 返回的 published action 决定。OpenAPI、RBAC、后端路由、Web SDK、TUI 和 checked-in action fixture 必须精确一致；不可用的 provider 能力不会发布，也不会显示一个点击后才返回 `UNSUPPORTED` 的入口。

| 页面 | 正式能力 |
| --- | --- |
| Store | Catalog 搜索、来源列表、注册和移除；Release 导入、校验、安装、升级、回滚和删除 |
| Topology | draft/revision、Endpoint/Link 编辑、validate、确定性 diff、apply、rollback、status、export |
| Deployments | RuntimeInstance 列表与详情、启动、停止、重启、卸载和真实健康 |
| Nodes | 一次性注册码、节点列表/健康、证书吊销、drain 和移除 |
| Operations | JSON plan、confirm、apply、cancel、retry、rollback、日志与可续传 SSE 事件 |
| Diagnostics | 对当前已应用 Topology 快照创建报告、列表、查看及 JSON/Markdown 导出 |

TUI 使用相同 action 和默认值。TUI 不复刻拖拽，但 Store、Topology、Deployment、Node、Operation、日志、重试、取消和诊断控制能力与 Web 等价。

## Store 工作流

Store 读取已经验证的 Catalog v2。生产 Catalog 包含 semver、channel、目标平台、最低编排器版本、依赖、metadata SHA-256、OCI RepoDigest 和 Ed25519 签名；Catalog 注册表使用 RFC 8785/JCS 验签。页面不会接受浮动 tag 作为生产安装目标。

“仅导入”只持久化 Release；Docker 调用数必须为零。“安装”默认使用 `Managed + start=true`，并显式选择 `target_node_id`。请求被接受只显示 Operation ID；只有 Agent 完成拉取、digest 校验、容器创建/启动、健康门禁和投影提升后才显示 `RUNNING/HEALTHY`。

升级和回滚保留旧 RuntimeInstance，待新实例健康并完成 provider/Gateway 切换后再移除旧实例。失败时 Operation 显示补偿或 `NEEDS_ATTENTION`，页面不会把 imported、planned、deferred 或 HTTP 2xx 当作 installed/running。

Release 声明的 Auth、Gateway、migration、config/secret、Redis、storage、frontend 和 API registry 是类型化 pipeline 步骤。目标 Node 未声明所需 provider 时，plan 阶段直接拒绝。External 安装不创建容器，必须先通过声明的 endpoint 健康检查。

## Topology 工作流

画布只能引用已经注册或部署的服务。拖入节点和连接 Endpoint/Link 只产生 draft revision；`apply` 才创建异步 Operation。Revision 不可变，rollback 会复制历史 Spec 生成一个新 revision，再走正常 apply saga。

画布颜色、部署状态、链路状态和 drift 来自 `TopologyStatus`，不从 Endpoint/Link 的期望字段推断。applied head 只在所有必要步骤成功后推进；补偿失败显示 `Degraded`，reconciler 会继续对账。

坐标通过 `GET/PUT /api/v1/ui/layout?topology_id=...` 按用户、拓扑持久化，不写入 TopologySpec。旧 `.ojos/ui-layout.json` 只导入一次；保存失败会在页面显示，不会静默吞掉。

## Operation 与事件

`plan` 返回 `201`；install/apply/rollback、Deployment 生命周期等异步 mutation 返回 `202 + operation_id`。Web 自动为所有 mutation 生成 `Idempotency-Key`，集合接口跟随 cursor，Topology revision mutation 使用 ETag/`If-Match`。

Operation 日志与状态来自 `text/event-stream`。客户端保存最后一个事件 ID，重连时发送 `Last-Event-ID`；每批事件和响应体都有上限。关闭日志面板、离开页面或页面隐藏时会取消请求并停止重连。

成功响应读取 `data` 和 `meta.request_id`；失败读取 `application/problem+json`。`FAILED`、`BLOCKED`、`NEEDS_ATTENTION` 或兼容适配器的失败结果不会被包装成成功提示。429/503 的 `Retry-After` 会保留给客户端。

## 身份与会话

Desktop 把一次性 bootstrap secret 放在宿主与 backend 之间，仅用于换取 HttpOnly、SameSite 本地 admin 会话；secret 只能消费一次。远程 Web 使用 OIDC Authorization Code + PKCE。两种模式下 bearer/OIDC token 和 Desktop secret 都不进入 `localStorage` 或 `sessionStorage`，mutation 使用会话绑定的 CSRF header。

页面权限固定为 viewer/operator/admin。viewer 只读；operator 执行日常运行操作；admin 管理 Catalog、Node 身份和高风险变更。后端在外部副作用前持久化 append-only 审计 intent，审计不可写时页面收到 problem response，操作不会被派发。

## 防卡死约束

- 启动和普通读取有硬超时，并支持 `AbortSignal`。
- 全局刷新为 single-flight；旧 generation 的响应不能覆盖新状态。
- 页面隐藏时暂停轮询，重新可见后立即刷新。
- SSE、事件解析、集合分页、日志和内存缓存都有数量/字节上限。
- `ResizeObserver` 只在尺寸真实变化时更新，卸载时清理 observer 和 timer，避免 render/ref 递归。
- Store/Topology mutation 不阻塞刷新循环，Operation 状态独立追踪。

Vitest 覆盖 SDK、身份、能力矩阵、状态竞态、画布和 SSE；Playwright 覆盖 Store/Topology 全流程、RBAC 拒绝、失败补偿、布局失败、Operation 重试/取消，以及持续运行。GA 门禁设置 `OJOS_E2E_SOAK_MS=1800000`，要求 Desktop/Web 连续运行 30 分钟仍可响应且没有轮询并发增长。

## 代码位置

```text
manager/web/src/
  api.ts                  /api/v1 envelope、problem、cursor、ETag、SSE
  auth.ts                 Desktop/OIDC 会话与内存 CSRF
  published-actions.ts    Web published action fixture
  store.ts                Pinia 状态、single-flight 刷新、布局保存
  components/
    FlowCanvas.vue
    OperationLogs.vue
  views/
    StoreView.vue
    TopologyView.vue
    ServicesView.vue
    NodesView.vue
    OperationsView.vue
    DiagnosticsView.vue
```

生产环境变量、provider 配置、备份恢复和发布门禁见 `docs/orchestrator/operations-v1.md`。
