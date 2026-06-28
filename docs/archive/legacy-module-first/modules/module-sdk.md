# 模块 SDK

Module SDK 是普通 OJOS 模块的开发入口，由 Module Contract v1、`ojosctl`、`.ojosmod` package、compatibility harness 和文档组成。

## 创建模块

```powershell
cargo run -p ojosctl -- module init ojos.sample-hello --name "Sample Hello" --kind feature --out modules/sample-hello --with-topology
```

脚手架默认 metadata-only，不生成 hook、script、dynamic frontend bundle、Docker image、host mount 或 privileged runtime option。

## 校验与打包

```powershell
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
```

临时 package 只能写入 `.tmp/agent/`，不能提交。

## 安装与启用

v0.1.0 官方安装入口是原生工具：

```powershell
cargo run -p ojosctl -- module install-plan modules/sample-hello/module.yaml
cargo run -p ojosctl -- module install modules/sample-hello/module.yaml --dry-run
cargo run -p ojosctl -- module enable ojos.sample-hello
cargo run -p ojosctl -- module disable ojos.sample-hello
cargo run -p ojosctl -- module uninstall-dry-run ojos.sample-hello
cargo run -p ojos-installer-tui --
```

Gateway admin API 仍由 compatibility harness 使用，用于 live control plane install/apply/enable/disable 验收。Web Shell 只作为管理视图和状态视图，不作为官方安装器主入口。

## 查看 Runtime

```powershell
cargo run -p ojosctl -- runtime snapshot
cargo run -p ojosctl -- runtime routes
cargo run -p ojosctl -- runtime services
cargo run -p ojosctl -- runtime service problem-api
```

管理 API：

```text
GET  /api/admin/modules/runtime-snapshot
GET  /api/admin/modules/runtime-snapshot?include_disabled=true
GET  /api/admin/modules/runtime/routes?include_disabled=true
GET  /api/admin/runtime/services
POST /api/admin/runtime/services/:id/plan-start
```

## Controlled Apply

可信 managed compose service 可生成 plan 后由 `ojosctl` apply：

```powershell
cargo run -p ojosctl -- runtime plan-restart problem-api --out .tmp/agent/scratch/problem-api-restart.json
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --dry-run
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --confirm
```

Metadata service 不能 apply。Gateway/Web 不执行 apply。

## v1 禁止事项

- 禁止 arbitrary `target_url`。
- 禁止 `command`、`script`、`hook`、`postinstall`、`preinstall`。
- 禁止 arbitrary `image`、`mount`、`host_path`、`privileged`、`cap_add`。
- 禁止动态执行不可信 frontend JavaScript。
- 禁止绕过 Gateway auth mode。
- 禁止覆盖 reserved prefix。
- 禁止 remote module market。
