# Kernel Installer

> 文档状态：当前实现，v0.1.0 发布基线
> 适用范围：Installer 开发、部署、安全审计、模块作者
> 最后更新：2026-06-28

Installer 是 OJOS Kernel 能力，不属于普通业务模块。它负责 manifest 校验、package 校验、生命周期计划、operation history 和受控 runtime apply 边界。

## 源码位置

```text
kernel/installer/core     纯逻辑核心
kernel/installer/service  内部 HTTP service
kernel/installer/cli      ojosctl 原生命令行
kernel/installer/tui      ojos-installer-tui 原生终端界面
```

`core` 不依赖 Gateway 或 Web Shell。`service` 只在 compose 内部网络提供 API。`cli` 和 `tui` 是 v0.1.0 官方原生安装入口。Web Shell 只作为管理视图。

## 原生安装入口

CLI：

```powershell
cargo run -p ojosctl -- doctor
cargo run -p ojosctl -- status
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- runtime snapshot
cargo run -p ojosctl -- runtime routes
```

TUI：

```powershell
cargo run -p ojos-installer-tui --
```

TUI 是 `ratatui + crossterm` 原生终端界面，不是浏览器、Electron 或 WebView。它支持首页状态、模块列表、Runtime 服务、操作历史、拓扑文本视图、计划视图、搜索过滤和帮助页。

## Adapter 边界

- Gateway 是 admin HTTP adapter，负责 JWT、权限和内部 service 调用。
- Web Shell 是 frontend shell，只查看状态、计划、Runtime、routes、services、operations、topology 和贡献元数据。
- `ojosctl` / `ojos-installer-tui` 是官方安装器入口。
- Compose 是受信任本地 runtime deployment adapter。
- Installer Core 不依赖这些 adapter，也不读取前端代码。

## 安全边界

v0.1.0 不支持：

- remote module market。
- hook、postinstall、preinstall 或任意 script 执行。
- dynamic untrusted frontend bundle。
- manifest 提供 arbitrary target URL。
- manifest 提供 arbitrary image、mount、host_path、privileged、cap_add。
- Gateway/Web 直接 runtime apply。
- package signature / publisher trust policy。

`.ojosmod` v1 只提供 checksum integrity，不证明发布者可信。

## Runtime Wiring

Installer 写入 module registry 和 stored manifest。Kernel Module Runtime 从 registry 和 manifest 派生 Runtime Snapshot。Gateway 和 Web Shell 读取 Runtime Snapshot，不需要为普通 metadata/module contribution 修改核心逻辑。

`gateway_routes.service_id` 是 manifest 合同中的服务标识。兼容 DB 字段 `target_service` 仍表示 service id，不表示 URL。Installer 拒绝 `target_url`，Gateway 只通过 trusted service map 解析 upstream。

## Runtime 命令

```powershell
cargo run -p ojosctl -- runtime services
cargo run -p ojosctl -- runtime service problem-api
cargo run -p ojosctl -- runtime plan-restart problem-api --out .tmp/agent/scratch/problem-api-restart.json
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --dry-run
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --confirm
cargo run -p ojosctl -- runtime operations
cargo run -p ojosctl -- runtime operation <operation_id>
```

真实 apply 必须显式 `--confirm`。`--dry-run` 不执行 compose。计划会重新校验 action、TTL、compose file、trusted allowlist、target service 和 argv 形状。输出会裁剪并脱敏。

## Module SDK 命令

```powershell
cargo run -p ojosctl -- module init ojos.sample-hello --name "Sample Hello" --kind feature --out modules/sample-hello --with-topology
```

脚手架默认 metadata-only，不生成 hook、script、dynamic frontend bundle、target URL、image、mount 或 privileged 配置。已存在目录默认拒绝覆盖，除非显式 `--force`。
