# OJOS

OJOS 是模块化 Online Judge 系统。当前仓库正在进行全仓人工审计与清理，`v0.1.0` 只能作为待验收目标，不声明已经达到发布标准。

本仓库仍处于安全边界审计中，模块热插拔只完成到已记录的 L0/L1/L2 foundation 范围，Contest 相关能力尚未实现。

## 当前能力

- 认证、题库 API、评测 API、Judge Worker。
- Kernel Installer Core：manifest 校验、package/verify、install plan、enable/disable plan。
- Module Registry 和 Runtime Snapshot v1。
- Dynamic Gateway route table 和受信任 dynamic proxy。
- Web Shell contribution registry：菜单、路由元数据、权限、拓扑和模块贡献视图。
- Service Runtime Driver foundation 和 `ojosctl` controlled apply。
- Module Contract v1、Module SDK、`ojosctl module init`、`modules/sample-hello`。
- 原生安装入口：`ojosctl` 和 `ojos-installer-tui`。

## 未完成边界

- Contest 尚未实现。
- L3 dynamic frontend bundle 未完成。
- hooks 未实现。
- remote module market 未实现。
- package signature / trust policy 未完成。
- true multi-machine runtime apply 未完成。
- 完整模块热插拔自动化未完成。
- Judge Core 不标记通用可用状态。

## 本地启动

复制 `.env.example` 为 `.env` 并替换 secret 占位值，然后启动控制面：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build
```

Gateway 默认监听：

```text
http://localhost:8080
```

## 原生安装器

正式安装、打包、验证、启用、禁用和 runtime apply 使用原生入口：

```powershell
cargo run -p ojosctl -- doctor
cargo run -p ojosctl -- status
cargo run -p ojos-installer-tui --
```

Web Shell 中的 Installer 页面只作为管理视图，不是官方安装器主入口，也不执行危险 apply。

## Module SDK

创建 metadata-only 模块：

```powershell
cargo run -p ojosctl -- module init ojos.sample-hello --name "Sample Hello" --kind feature --out modules/sample-hello --with-topology
```

校验、打包、验证：

```powershell
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
```

临时 package 和 plan JSON 只能放在 `.tmp/agent/`，不能提交。

## Runtime Controlled Apply

Gateway 和 Web Shell 不 apply runtime plan。受控 apply 使用：

```powershell
cargo run -p ojosctl -- runtime plan-restart problem-api --out .tmp/agent/scratch/problem-api-restart.json
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --dry-run
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --confirm
```

真实 apply 必须显式确认，并且只能操作 trusted compose allowlist service。

## 验收

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -RunControlledApply -SkipDockerBuild
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

完整发版还需要重新通过 e2e、Go、Rust、judge-worker、frontend、npm audit、release artifact 构建和人工审计结论。

## 发布产物

```powershell
powershell -NoProfile -File scripts\build-release-artifacts.ps1 -Version v0.1.0
```

默认输出到 `.tmp/release/v0.1.0/`，该目录不进入 Git。

## 文档

- [文档索引](docs/docs-index.md)
- [文档状态](docs/docs-status.md)
- [v0.1.0 候选发布说明](docs/release/v0.1.0-release-notes.md)
- [v0.1.0 发版清单](docs/release/v0.1.0-ship-checklist.md)
- [v0.1.0 已知限制](docs/release/v0.1.0-known-limitations.md)
