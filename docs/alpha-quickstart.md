# v0.1.0 Alpha 快速上手

这份说明只对应 2026-07-03 发布的 `v0.1.0-alpha`。该版本提供 daemon、TUI 和原生 GUI；当前源码已经删除
原生 GUI，改用 daemon 托管的 Web UI。不要用本文判断当前代码的入口或功能。

该版本可以从 URL 下载服务发布包，校验 SHA-256，并注册 Service、Endpoint、Route 和 Permission。发布包只含
服务契约，不含服务二进制或镜像。

## 1. 下载与解压

从 [GitHub Release](https://github.com/thedyingkai/Untitled-OJ/releases) 下载对应平台的 bundle：

- Windows：`ojos-orchestrator-v0.1.0-alpha-windows-x64.zip`（daemon / GUI / TUI）
- Linux：`ojos-orchestrator-v0.1.0-alpha-linux-x64.tar.gz`（daemon / TUI，不含 GUI）

解压后进入 bundle 目录。`platform/`、`services/` 和 `sets/` 是 daemon 读取的契约数据，因此下面使用
`--repo-root .`。发布页的 `manifest.json` 可用于核对每个资产的 SHA-256。

## 2. 运行编排器，按 service 看效果

不需要数据库：未设置 `ORCHESTRATOR_DATABASE_URL` 时编排器使用内存 store。

> **数据会丢**：内存 store 只存在于进程中。daemon 退出后，拓扑、Endpoint、Link 和 Operation 记录都会消失。
> 要保留数据必须配 `ORCHESTRATOR_DATABASE_URL` 指向一个已建表的 PostgreSQL（schema 见
> `services/orchestrator/migrations/`）：
>
> ```bash
> ORCHESTRATOR_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/ojos_orchestrator?sslmode=disable \
>   ojos-orchestrator-daemon --repo-root . --bind 127.0.0.1:8090
> ```
>
> 远程使用时还应设置 `ORCHESTRATOR_INTERNAL_TOKEN`，调用受保护接口时在
> `x-ojos-orchestrator-token` 请求头中携带同值。

```bash
# daemon（HTTP 控制面）
ojos-orchestrator-daemon --repo-root . --bind 127.0.0.1:8090

# 健康检查（store 会显示 "memory"）
curl http://127.0.0.1:8090/health

# 查看它管理的所有 service（这就是"按 service 看效果"）
curl http://127.0.0.1:8090/services
```

Windows bundle 的图形入口和两个平台的终端入口如下：

```bash
ojos-orchestrator-gui --repo-root .    # 仅 Windows bundle
ojos-orchestrator-tui --repo-root .
```

GUI 和 TUI 可以浏览 Service、Endpoint、Link、Topology、Operation，并执行该版本支持的 action。

## 3. 下载并注册服务契约

编排器可从 URL 拉取 zip 或 tar.gz，校验 SHA-256 后注册服务。该能力默认关闭：

```bash
# 开启 release 包加载后启动 daemon
ORCHESTRATOR_RELEASE_PACKAGE_LOAD=1 ojos-orchestrator-daemon --repo-root . --bind 127.0.0.1:8090
```

Release 里同时提供每个 service 的可下载包和 `manifest.json`（含每个包的 sha256）。用 `release.install` 拉取
（`source_url` 指向该包，`release_checksum` 取自 `manifest.json`）：

```bash
curl -X POST http://127.0.0.1:8090/releases/gateway/install \
  -H 'Content-Type: application/json' \
  -d '{
    "source_url": "https://github.com/thedyingkai/Untitled-OJ/releases/download/v0.1.0-alpha/ojos-service-gateway-v0.1.0-alpha.tar.gz",
    "release_checksum": "sha256:<从 manifest.json 复制 gateway 的值>",
    "confirm": "true"
  }'
```

成功时返回的 `action_result.changed_objects` 会包含 `ServiceRelease:gateway@0.1.0`、`Service:gateway`、
`Endpoint:...:gateway`、`Route:...`、`Permission:...` 等，表示该 service 已被拉取并注册。之后
`curl http://127.0.0.1:8090/services` 可看到它。

下载器支持 HTTP/HTTPS，最多跟随 5 次重定向，超时为 15 秒，压缩包上限为 64 MiB，并校验 SHA-256 与 manifest
一致性。

## 4. 已知边界（alpha）

- 拉取注册的是服务契约（`release.yaml`），不会下载或运行服务二进制、镜像。
- 判题（judge-worker）需要 **Linux + nsjail + cgroup v2 + 特权容器**，Windows 桌面上多半跑不起来；作为
  Linux Docker 镜像使用（`services/judge-worker/Dockerfile`）。
- 当前代码的 Web UI、商店和生命周期说明见 [Web UI 与插件商店](orchestrator/web-ui.md)。完整平台部署见
  [部署清单](ops/deployment-checklist.md)。
