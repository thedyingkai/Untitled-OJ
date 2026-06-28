# Kernel Overview

> 文档状态：当前实现，v0.1.0 发布基线
> 适用范围：Kernel 开发、模块开发、架构评审
> 最后更新：2026-06-28

OJOS Kernel 负责模块系统和运行时的系统级能力。Kernel 不包含 Contest、Training、Discussion、Remote OJ 等业务功能。

## Kernel 能力

```text
Module Installer
Module Registry
Module Runtime
Module Lifecycle
Module Topology
Module Health
Module Policy
Module Audit
Module Config
Module Package Verification
Module Dependency Resolver
Module Operation Lock
Module Operation History
Native Installer CLI/TUI
```

## Kernel Built-ins

```text
ojos.kernel.installer
ojos.kernel.module-runtime
ojos.kernel.module-registry
ojos.kernel.topology
ojos.kernel.policy
ojos.kernel.audit
ojos.kernel.config
ojos.kernel.health
```

Kernel built-ins 不允许普通 disable 或 uninstall。

## Platform Built-ins

```text
ojos.platform.gateway
ojos.platform.web-shell
ojos.platform.identity-access
ojos.platform.storage
ojos.platform.observability
```

Platform built-ins 默认受保护，不作为普通 feature module 卸载。

## 当前 Feature Module

```text
ojos.judge-core
ojos.demo-module
ojos.sample-hello
```

Judge Core 是第一个核心 feature module，但不标记 GA。Demo 和 Sample 用于 installer/runtime/sdk 验收，不代表真实业务模块。

## Runtime Wiring v1

Runtime Snapshot 是模块贡献的运行态入口。Kernel Installer、Registry、Runtime 和 Topology 负责计算模块贡献；Gateway 和 Web Shell 只是读取该运行态表面。

当前普通模块可通过 manifest/package/runtime 贡献：

- permissions
- roles
- menus
- frontend route metadata
- gateway route metadata
- services/workers metadata
- health checks
- components
- storage buckets
- admin panels metadata
- topology nodes/edges

## Hotplug 当前状态

- L0 Metadata Hotplug：完成。
- L1 Route/Menu/Topology/Permission Hotplug：基本完成，使用 trusted route table、safe contribution registry 和权限过滤。
- L2 Service Runtime Foundation + Controlled Apply：foundation 完成，`ojosctl` 可对 trusted compose allowlist 计划执行 dry-run/confirm。
- L3 Dynamic Frontend Extension：未完成，不加载不可信动态 JS。
- L4 Full Module Hotplug：未完成，不提供 remote market、hook 或任意 service image 部署。

## 安全边界

- Gateway/Web 不挂载 Docker socket。
- Gateway/Web 不执行 Docker 或 shell 命令。
- Module manifest 不能声明 `command`、`script`、`hook`、arbitrary image、host mount、privileged 或 `cap_add`。
- Compose apply 只允许 fixed compose file 和 trusted service allowlist。
- Runtime apply 必须有 confirm、dry-run、lock、timeout、operation history 和 redaction。
- Web Shell Installer 页面只作为管理视图，官方安装入口是 `ojosctl` 和 `ojos-installer-tui`。

## 明确未完成

- Contest 尚未实现。
- remote module market 未实现。
- hook execution 未实现。
- dynamic frontend bundle 未实现。
- full hotplug 未完成。
- package signature / trust policy 未完成。
- true multi-machine runtime apply 未完成。
- Judge Core 不标记 GA。
