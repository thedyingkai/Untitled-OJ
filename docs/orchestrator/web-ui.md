# Orchestrator Web UI 与插件商店

Web UI 是编排器的浏览器入口。daemon 直接托管 `manager/web/dist`，页面与控制面 API 同源。前端使用
Vue 3、TypeScript、Vite 和 Pinia；拓扑画布位于 `src/components/FlowCanvas.vue`。

## 构建与访问

```bash
cd manager/web
npm ci
npm run typecheck
npm run build
```

Node.js 版本需满足 `^22.18.0 || >=24.11.0`；CI、release 和 Docker 构建固定使用 24.11。启动 daemon 后访问
`http://<bind>/`，也可用 `--web-root` 指定其他产物目录。`npm run dev` 默认监听 Vite 的 5174 端口，并把 API
代理到 `127.0.0.1:8090`。

产物不入库（`.gitignore` 的 `dist/` 覆盖 `manager/web/dist`），由交付链在构建时产出：

| 交付方式 | 产物来源 |
| -------- | -------- |
| Docker 镜像 | `services/orchestrator/backend/Dockerfile` 使用 `node:24.11-bookworm-slim` 构建，再拷入 `/app/manager/web/dist` |
| 后续 tag bundle | `deploy/release/pack-alpha.sh` 把 `manager/web/dist` 放进 bundle；已发布的 `v0.1.0-alpha` 早于本次改造，不含 Web UI |
| CI 校验 | `.github/workflows/orchestrator-ci.yml` 的 `orchestrator-web-build` job：`npm ci` + `npm run typecheck` + `npm run build` |

## 页面

| 页面 | 能力 |
| ---- | ---- |
| 拓扑 | 画布拖拽：从服务面板拖服务入画布创建 Endpoint；从节点右侧端口拖线到另一节点创建 Link；节点坐标经 `PUT /ui/layout` 持久化到 `.ojos/ui-layout.json`；选中节点/边可做健康检查与删除 |
| 商店 | 索引模块、GitHub Release 资产选择、手动 URL 导入、安装状态和卸载 |
| 服务 | 按 `host + service` 展示部署实例、部署状态、最近 Endpoint 检查和检查配置；启停重启会携带准确的主机、Endpoint 与版本 |
| 操作 | Operation 列表、确认 / 执行 / 回滚、日志实时刷新 |

操作页只在记录状态为 `SUCCEEDED` 或 `FAILED`，且 daemon 明确返回
`rollback_available=true` 时显示回滚按钮。这个字段由非空的 `rollback_plan.steps` 派生；目录预览、目录错误、
空计划和旧 daemon 的缺省响应都按不可回滚处理。

## 商店工作流

1. daemon 从 `OJOS_STORE_INDEX_URL` 读取索引，默认值为仓库内的 `store/index.json`。远程索引缓存 60 秒，
   `?refresh=1` 可强制刷新。
2. 安装时 `POST /store/install`：
   - `source_url` → core `import_external_release`：下载 release 包（http/https 跟随重定向，支持 GitHub Release 302；zip / tar.gz / 裸 release.yaml），解出 release.yaml，校验后合成 ServiceManifest 并注册 Service + Release 两条记录；
   - 随后派发 `release.install` 动作（`confirm=true`），走既有 Operation 计划/执行/回滚链路。
3. 请求可带 `version`、`checksum`、`host_ip`、`gateway_node_id`、`execute_service_driver` 和
   `external_service_running`。同一服务有多个 release 时必须明确版本。默认索引的本地条目已经填写各自
   `release.yaml` 的 sha256；daemon 强制校验时，页面会把 checksum 标为必填并拒绝空值提交。

“已安装”只根据 `HostService` 部署记录判断，不再把仓库中可见的 manifest 当成已安装模块。一个服务在多台
主机上的记录会保留在 `deployments` 数组中，包括各自的版本、主机和状态。

## 运行时驱动与交付物

页面默认不授权运行时驱动。Service、Operation 和商店页面只有在用户勾选
`execute_service_driver` 后，才会运行本地进程或 Docker Compose 命令；回滚需要再次授权。首次安装可以只写
编排状态，但已有 running 固定运行时的升级必须授权，执行器会先停止旧版本，避免 PID 或容器被遗留。

若服务已经由控制面之外的系统启动，安装请求改传 `external_service_running=true`。它不能与 driver 授权同时
使用，Endpoint 必须可达，并且不能覆盖仍可能活动的 local、node 或 external 旧部署。成功登记后会写入
`runtime_owner=external`；Service 页的启停、重启和卸载会拒绝用本地 driver 操作这类部署。应先在真实 owner
一侧停止或移除运行时，再处理控制面记录。

商店包和 alpha bundle 主要携带 `release.yaml`、`service.yaml`、schema 与 Web 产物。最小 daemon Docker
镜像也只保留这些运行时数据，不含 Go 源码、Compose 文件、Docker CLI 或业务镜像。因此：

- 已经在外部启动的服务用 `external_service_running=true` 登记，`ExternalEndpointDriver` 只负责受支持的元数据和健康动作；
- `LocalProcessDriver` 需要发布声明中的命令、工作目录和相应源码或二进制；
- `DockerComposeDriver` 需要 Compose 文件、Docker CLI、daemon 访问权限和相关镜像。

要执行后两种驱动，应从完整源码工作区运行 daemon，或把经过审核的运行资产挂载进交付环境。缺少资产时驱动会
失败，不应把“契约已导入”理解为“服务已启动”。

## 相关环境变量

| 变量 | 作用 |
| ---- | ---- |
| `OJOS_STORE_INDEX_URL` | 商店索引地址（http(s) 或仓库内相对路径） |
| `OJOS_GITHUB_TOKEN` / `GITHUB_TOKEN` | GitHub API 令牌（可选，提升配额、访问私有仓库） |
| `ORCHESTRATOR_RELEASE_PACKAGE_LOAD` | `1` 时 release.install 真实下载包并开放 `/store/import`、`/store/install`；未启用时这两个商店端点直接返回 403 |
| `ORCHESTRATOR_RELEASE_PACKAGE_ROOT` | 包缓存根（默认 repo root，缓存位于 `.orchestrator-release-cache/`） |
| `ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM` | 生产应设为 `1`；所有 release 加载入口都必须提供请求 checksum 或 manifest checksum |
| `ORCHESTRATOR_ALLOW_PRIVATE_RELEASE_SOURCE` | `1` 时允许包源指向 loopback / 私网 / CGNAT（内网私有镜像源场景）。link-local（含云元数据 `169.254.169.254`）与 multicast/broadcast 等**始终拒绝**，此开关不放行 |
| `ORCHESTRATOR_INTERNAL_TOKEN` | 控制面令牌。配置后除 `GET /health` 与静态资源外所有 API 都要求请求头 `x-ojos-orchestrator-token`；Web UI 首次收到 401 会弹出令牌输入框，值存浏览器 localStorage |
| `ORCHESTRATOR_MAX_WORKERS` | daemon 工作线程数（默认 32）；有界连接队列容量为 64，满时返回 503 |

daemon 对静态页面和 API 响应都发送 `frame-ancestors 'none'`、`X-Frame-Options: DENY`、
`X-Content-Type-Options: nosniff` 与 `Referrer-Policy: no-referrer`，避免控制页被第三方站点嵌入后诱导点击。
控制令牌保存在当前浏览器的 localStorage；只应在受信任的操作终端使用，退出共享终端前要清除站点数据。

## 出网安全

商店链路会以 daemon 身份发起 HTTP 请求，因此下载前一律经过 `validate_outbound_url`（core `store.rs`）：只放行 http/https；域名解析后逐个 IP 校验；重定向不交给 HTTP 客户端自动跟随，而是最多手动跟 5 跳、**每跳重新校验**，且 GitHub 令牌只在第一跳发送。解压有预算上限（总量 512MB、条目 5000、单条目 64MB），超限即中止并清理缓存目录，压缩包本身上限 64MB。

仍有 DNS rebinding 风险：校验和连接会分别解析域名，DNS 记录可能在两次解析之间改变。HTTP 客户端禁用系统
代理，因此不能把出口代理当作第二道过滤。

## 商店索引格式

```json
{
  "schema_version": 1,
  "modules": [
    {
      "id": "judge-api",
      "name": "Judge API",
      "description": "…",
      "kind": "backend-api",
      "tags": ["oj"],
      "repo": "owner/repo",
      "source_url": "https://github.com/owner/repo/releases/download/v1.0.0/judge-api-1.0.0.zip",
      "checksum": "sha256:…"
    }
  ]
}
```

`repo` 与 `source_url` 至少一项：有 `repo` 时 UI 走 GitHub Release 资产选择；否则用 `source_url` 直装。
索引中的 checksum 必须对应实际下载体；本地目录源对应其 `release.yaml` 文件。模块包用
`deploy/release/pack-service-package.sh <service>` 生成。

## daemon 新增 API

| 方法与路径 | 说明 |
| ---- | ---- |
| `GET /store/status` | 索引地址、包加载开关、token 配置状态 |
| `GET /store/index` | 索引与实际 HostService 安装记录的对照 |
| `GET /store/github/releases?repo=owner/name` | GitHub Release 与资产列表 |
| `POST /store/import` | 仅导入注册，不安装 |
| `POST /store/install` | 导入（可选）+ 派发 release.install |
| `GET/PUT /ui/layout` | 画布布局持久化 |

静态托管：GET 未命中 API 前缀时按 `--web-root` 提供文件，`assets/` 目录带 immutable 缓存头；产物缺失时根路径返回引导页。
