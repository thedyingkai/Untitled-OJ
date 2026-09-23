# OJOS Orchestrator Web UI

这是编排器的同源 Web UI，生产入口由 Desktop WebView 或远程控制面托管，不要求打开外部浏览器。它提供版本化拓扑、Catalog v2 Store、Deployment、Node 和 Operation/SSE 控制能力，使用 Vue 3、TypeScript、Vite 与 Pinia。

## 环境和构建

Node 版本需满足 `^22.18.0 || >=24.11.0`，npm 至少为 10。CI 和 Docker 使用 Node 24.11。

在 `manager/web` 目录构建产品：

```bash
npm ci
npm run typecheck
npm run build
```

产物在 `dist/`。daemon 默认读取 `<repo-root>/manager/web/dist`，也可用 `--web-root` 指向其它目录。

从仓库根目录启动：

```bash
cargo run -p ojos-orchestrator-daemon -- \
  --repo-root . \
  --bind 127.0.0.1:8090
```

上述方式只用于 Web 开发。Desktop 正式入口会启动随机 loopback 端口并在 WebView 内加载；生产 daemon 必须使用持久存储，UI 不把内存模式显示成可上线状态。

## 开发

```bash
npm run dev
```

Vite 监听 `127.0.0.1:5174`，将控制面路径代理到 `127.0.0.1:8090`。开发服务器只负责前端热更新，daemon 仍需单独运行。

## 外部验证

单元测试、浏览器场景及其控制面夹具均在仓库外的独立验证目录维护。本产品 package 不包含测试脚本和测试依赖。外部验证复制当前 Web 源码，在独立工作目录中安装自己的验证依赖；不要将夹具或测试文件放回 `src`、workflow 或 Git 工作树。

## 会话和 API 契约

业务请求和按用户/拓扑隔离的布局状态都只使用 `/api/v1`（布局为 `/api/v1/ui/layout?topology_id=<selected>`，禁止回退到固定 `primary`）。成功响应必须包含 `data` 与 `meta.request_id`，错误按 `application/problem+json` 处理；所有 mutation 自动携带 `Idempotency-Key`，Topology revision mutation 额外携带当前 draft 的强 ETag `If-Match`，集合读取跟随 `next_cursor`。

Topology 页以 Deployment ID 和 Endpoint ID 关联画布状态；同一 `service_id` 在不同 Node 上的实例不会合并。Link 的 ApiBinding 可按 requirement 新增、编辑、显式 rebind 或解除，保存先产生 immutable draft，Apply 后才改变实际路由和凭据。Store 安装预览只接受服务端为本次安装返回的 prospective diff，并显示本次候选映射的 SHA-256 确认指纹；升级/回滚会在确认框中列出保留的 Binding、Topology CAS 和指纹。卸载遇到 `DEPLOYMENT_ACTIVE_BINDINGS` 时会列出需要先解除并 Apply 的 Link。

Desktop 使用同源一次性 bootstrap 会话；远程 Web 使用 OIDC Authorization Code + PKCE 和 HttpOnly 会话。页面不接收、不持久化 bearer token，`localStorage` 不参与身份认证。

## 运行时操作

启动、停止、重启、卸载、安装、Apply 和 Rollback 都是异步 mutation，只在服务端返回 `202 + operation_id` 后显示“已提交”。UI 不把接受请求显示成执行成功，最终状态与日志来自 Operation 投影和 SSE。

服务页读取 `/api/v1/deployments` 的持久 RuntimeInstance，一行对应唯一 `deployment_id`；生命周期请求精确指向该 ID。Store 安装固定选择 `target_node_id`，默认 `MANAGED + start=true`。未发布的 upgrade、rollback、Node drain/remove 等能力不会显示操作入口。

Topology 画布编辑 Endpoint/Link 时创建新的 immutable draft revision；validate、diff、apply、rollback 使用正式 Topology API。健康、链路状态和 drift 只读自 `TopologyStatus`，不会从 Spec 或 manifest 推断。

## 目录

```text
src/
  api.ts                 # /api/v1 envelope、problem、cursor、ETag 与 SSE
  auth.ts                # Desktop/OIDC 会话初始化、401 重定向与内存 CSRF
  endpoint.ts            # IPv4 / IPv6 Endpoint ID 解析
  store.ts               # Pinia 状态、4 秒轮询、布局保存
  flow-types.ts          # 画布节点和边类型
  components/
    FlowCanvas.vue       # 平移、缩放、拖拽和连线
    EndpointNode.vue     # Endpoint 节点卡片
    OperationLogs.vue    # 可续传的 Operation SSE 日志
  views/
    TopologyView.vue
    StoreView.vue
    ServicesView.vue
    NodesView.vue
    OperationsView.vue
```
