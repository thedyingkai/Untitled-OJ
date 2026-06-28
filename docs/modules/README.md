# 模块系统

> 文档状态：当前实现，v0.1.0 发布基线
> 适用范围：模块开发、Kernel/Runtime 维护、管理后台
> 最后更新：2026-06-28

OJOS 模块系统用于把 Kernel、Platform、Judge Core 和未来普通模块统一登记为可审计、可展示、可验收的系统能力。当前版本不声明 full hotplug 完成，也不包含 Contest 实现。

## 当前实现

| 能力 | 路径或入口 | 状态 |
| --- | --- | --- |
| Module Registry | `deploy/migrations/000009_module_registry.up.sql` | 已实现 |
| Module Installer operation history | `deploy/migrations/000010_module_installer.up.sql` | 已实现 |
| Installer Core | `kernel/installer/core` | 已实现 |
| Installer Service | `kernel/installer/service` | 已实现，内部 HTTP service |
| CLI | `kernel/installer/cli` / `ojosctl` | 已实现 |
| Native TUI | `kernel/installer/tui` / `ojos-installer-tui` | 已实现 |
| Runtime Snapshot | `/api/admin/modules/runtime-snapshot` | 已实现 |
| Runtime Routes | `/api/admin/modules/runtime/routes` | 已实现 |
| Runtime Services | `/api/admin/runtime/services` | 已实现 |
| Web Shell 管理视图 | `frontend/src/views/admin/*` | 已实现 |
| Sample Module | `modules/sample-hello` | 已实现，SDK 样例 |
| Contest | 无 | 未实现 |

## 当前模块

Kernel 模块：

- `ojos.kernel.installer`
- `ojos.kernel.module-runtime`
- `ojos.kernel.module-registry`
- `ojos.kernel.topology`
- `ojos.kernel.policy`
- `ojos.kernel.audit`
- `ojos.kernel.config`
- `ojos.kernel.health`

Platform 模块：

- `ojos.platform.gateway`
- `ojos.platform.web-shell`
- `ojos.platform.identity-access`
- `ojos.platform.storage`
- `ojos.platform.observability`

Feature / SDK 模块：

- `ojos.judge-core`
- `ojos.demo-module`
- `ojos.sample-hello`

## 分层关系

```text
Service / Worker / Component -> Module -> Set -> OJOS
```

Module 是 Installer、Runtime Snapshot、Topology、Permission Registry 和 Web Shell contribution 的最小声明单位。

## 安装器入口

官方原生入口：

```powershell
cargo run -p ojosctl -- doctor
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module install-plan modules/sample-hello/module.yaml
cargo run -p ojosctl -- runtime snapshot
cargo run -p ojos-installer-tui --
```

Web Shell 的 `/admin/modules/installer` 是管理视图，只展示校验、计划、健康和操作历史，不作为官方安装器主入口。

## Runtime 接入

模块启用后，Runtime Snapshot 会暴露该模块的 active contribution：

- permissions
- roles
- menus
- frontend route metadata
- gateway route metadata
- services
- workers
- storage buckets
- health checks
- components
- admin panels metadata
- topology nodes/edges

Disabled module 不进入 active snapshot。管理员可用 `include_disabled=true` 查看 registry contribution。

## 安全边界

- Manifest 不能携带 secret、token、password、target URL 或执行字段。
- Gateway dynamic proxy 只解析 trusted `service_id`。
- Web Shell 不动态加载不可信 JS。
- Gateway/Web 不执行 runtime apply。
- module-installer 不挂载 Docker socket。
- `ojosctl` controlled apply 只允许 trusted compose allowlist。
- Judge Core disable/uninstall 受保护，且不标记 GA。

## 验收

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\e2e-module-compat.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123
```

预期：

- `failed=0`
- `path_leaks=0`
- `sample_module_compat=passed`
- 普通用户 403
- 无 token 401

## 相关文档

- [Module Contract v1](module-contract-v1.md)
- [Module SDK](module-sdk.md)
- [Module Lifecycle](module-lifecycle.md)
- [Module Installer](module-installer.md)
- [Module Package Format](module-package-format.md)
- [Judge Core Readiness](judge-core-readiness.md)
- [Kernel Module Runtime](../kernel/module-runtime.md)
- [Admin API](../api/admin-api.md)
