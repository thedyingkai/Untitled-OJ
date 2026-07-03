# v0.1.0 Alpha 快速上手

本 alpha 提供**可执行文件形态的编排器**（daemon / GUI / TUI），可以直接运行查看它管理的 service，并能
**从 URL 拉取 service 发布包**并注册。

> 边界说明（如实标注）：拉取下载的是**服务契约 `release.yaml`**（下载 + sha256 校验 + 注册 service /
> endpoint / route / permission），**不是**服务的可运行二进制/镜像。判题需要 Linux + nsjail 环境，见文末。

## 1. 下载与解压

从 [GitHub Release](https://github.com/thedyingkai/Untitled-OJ/releases) 下载对应平台的 bundle：

- Windows：`ojos-orchestrator-v0.1.0-alpha-windows-x64.zip`（含 daemon / GUI / TUI）
- Linux：`ojos-orchestrator-v0.1.0-alpha-linux-x64.tar.gz`（含 daemon / TUI；Linux 不含 GUI）

解压后进入该目录。目录内同时包含 `platform/`、`services/`、`sets/` 运行时数据，因此下面用 `--repo-root .`。

## 2. 运行编排器，按 service 看效果

不需要数据库：未设置 `ORCHESTRATOR_DATABASE_URL` 时编排器使用内存 store。

```bash
# daemon（HTTP 控制面）
ojos-orchestrator-daemon --repo-root . --bind 127.0.0.1:8090

# 健康检查（store 会显示 "memory"）
curl http://127.0.0.1:8090/health

# 查看它管理的所有 service（这就是"按 service 看效果"）
curl http://127.0.0.1:8090/services
```

图形 / 终端入口（能力与 daemon 等价，见 [GUI / TUI 等价性](orchestrator/gui-tui-parity.md)）：

```bash
ojos-orchestrator-gui --repo-root .    # 原生 GUI（仅 Windows bundle 提供）
ojos-orchestrator-tui --repo-root .    # 终端 TUI
```

在 GUI/TUI 里可浏览 Service、Endpoint、Link、Topology、Operation，并执行 action。

## 3. 拉取 service 下载

编排器可从 URL 拉取 service 发布包（zip / tar.gz），校验 sha256 后注册该 service。该能力默认关闭，需要开启
开关：

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

> 实现说明：编排器的下载器支持 http/https（含一次重定向，兼容 GitHub Release 资源 URL），有 15s 超时、
> 64MiB 上限、sha256 校验和严格的 manifest 一致性校验。

## 4. 已知边界（alpha）

- 拉取注册的是服务契约（`release.yaml`），不下载/运行服务二进制或镜像。
- 判题（judge-worker）需要 **Linux + nsjail + cgroup v2 + 特权容器**，Windows 桌面上多半跑不起来；作为
  Linux Docker 镜像使用（`services/judge-worker/Dockerfile`）。
- 完整平台部署、数据播种与浏览器 OJ 站点见 [部署清单](ops/deployment-checklist.md) 与
  [项目完成度总结](completeness-summary.md)。
