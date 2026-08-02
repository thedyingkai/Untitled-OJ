# OJOS Orchestrator Web UI

这是编排器的浏览器入口，提供拓扑画布、Service 生命周期、插件商店和 Operation 日志。它使用 Vue 3、TypeScript、Vite 与 Pinia；`src/components/FlowCanvas.vue` 自己实现画布交互。

## 环境和构建

Node 版本需满足 `^22.18.0 || >=24.11.0`，npm 至少为 10。CI 和 Docker 使用 Node 24.11。

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

随后打开 `http://127.0.0.1:8090/`。未设置 `ORCHESTRATOR_DATABASE_URL` 时使用内存 store，daemon 退出后操作和拓扑记录会丢失。

## 开发

```bash
npm run dev
```

Vite 监听 `127.0.0.1:5174`，将控制面路径代理到 `127.0.0.1:8090`。开发服务器只负责前端热更新，daemon 仍需单独运行。

## 控制面令牌

生产环境应设置 `ORCHESTRATOR_INTERNAL_TOKEN`。配置后，除 `GET /health` 和静态资源外，所有 API 都要求请求头：

```text
x-ojos-orchestrator-token: <ORCHESTRATOR_INTERNAL_TOKEN>
```

Web UI 收到 401 后会显示令牌输入页。令牌只保存在当前浏览器的 `localStorage`，不会写回 daemon；共享机器用完后应从侧栏清除。

## 运行时操作

启动、停止、重启和卸载可能执行本地进程或容器命令。相关页面要求先勾选“授权执行运行时驱动”，未授权时按钮不可用。

服务页读取 `/deployments`，一行对应一条 `host + service` 部署记录。同一服务部署在多台主机时会分成多行；启动、停止和重启请求会同时携带该行的 `host_ip`、`endpoint` 与 `version`，不会再由后端任选一个 Endpoint。表中的“部署状态”来自 HostService，“最近检查”来自 Endpoint 记录；“检查配置”只说明协议与路径，不把 manifest 里的检查声明当成实时健康结果。

这个开关不负责提供运行资产。alpha bundle 和 Orchestrator 镜像只有控制面 binary、Web dist、schema、manifest、模板与商店索引，没有完整业务服务源码、Compose 文件或业务镜像。要执行 local-process/container driver，目标环境还需相应源码、binary、image 和配置。

## 目录

```text
src/
  api.ts                 # daemon REST 封装与 action 状态检查
  token.ts               # 控制面令牌和 401 状态
  endpoint.ts            # IPv4 / IPv6 Endpoint ID 解析
  store.ts               # Pinia 状态、4 秒轮询、布局保存
  flow-types.ts          # 画布节点和边类型
  components/
    FlowCanvas.vue       # 平移、缩放、拖拽和连线
    EndpointNode.vue     # Endpoint 节点卡片
    TokenGate.vue        # 401 令牌门禁
    OperationLogs.vue    # Operation 日志轮询
  views/
    TopologyView.vue
    StoreView.vue
    ServicesView.vue
    OperationsView.vue
```
