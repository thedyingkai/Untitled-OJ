# ADR：Module Installer 仓库边界

> 状态：已接受；路径归属被 [项目结构 v2、Kernel 与 Modules](adr-project-structure-v2-kernel-modules.md) 更新
> 日期：2026-06-27

## 背景

Module Installer 是 OJOS 的系统基础能力。它负责 manifest 校验、package 校验、依赖规划、模块生命周期、operation lock、operation history 和 audit record。它需要与当前 Control Plane 数据库、Gateway Admin API、Web Shell 管理视图、Docker Compose 部署和 Module Registry v0 schema 集成。

Installer 需要支持 v0 快速迭代，同时不能把未来独立拆仓变成高成本重写。

## 决策

OJOS 当前不把 Module Installer 立即拆到独立仓库。项目结构 v2 更新原始路径草案后，Installer 作为 Kernel-owned Rust source 放在 monorepo 内：

```text
kernel/installer/core/
kernel/installer/service/
kernel/installer/cli/
kernel/installer/tui/
```

这是“monorepo placement with independent-repository boundaries”：源码在主仓库，但边界按可独立发布组件维护。

Installer 代码不得依赖 Go service internals 或 frontend code。它的契约限定为：

- PostgreSQL schema 与 transactions。
- Internal HTTP API。
- Module manifest schema。
- Module package format。
- Stable JSON request/response models。

## 方案比较

### 方案 A：直接保留在 OJOS monorepo

优点：

- 与当前 DB schema、Gateway、Web Shell 和 Compose stack 集成最简单。
- 保持单一 CI 与版本流。
- 不需要单独发布或 version pinning。
- 适合 v0 到 v1 的快速迭代。

缺点：

- 如果 Installer 未来被 OJOS 外部复用，会继承主仓库耦合。
- 独立基础组件的边界不够显眼，需要文档和 CI 约束。

### 方案 B：立即拆到独立仓库

候选名称：

```text
ojos-installer
ojos-module-installer
ojos-module-runtime
```

优点：

- 架构边界最清晰。
- CLI、library 和 service 可独立发布。
- 长期复用路径更直接。

缺点：

- 当前 schema 和 manifest contracts 仍在稳定。
- 集成成本立即上升。
- CI、release、version pinning 和 cross-repo compatibility 需要提前解决。
- 可能拖慢 Control Plane v0 发布路径。

### 方案 C：monorepo 内独立 Rust workspace

优点：

- 保持 v0 集成速度。
- 形成真实 library/service/CLI/TUI 边界。
- Rust APIs 清晰且可测试。
- 未来可迁移到独立仓库。
- 避免依赖 Go 或 frontend implementation details。

缺点：

- 需要持续维护显式契约。
- 拆仓前 CI 仍在主仓库运行。

## 后果

Installer workspace 应保持可迁移性。所有直接集成点必须被文档和测试覆盖：

- DB tables 与 migrations。
- Internal API endpoints。
- Manifest schema。
- Package format。
- Gateway Admin API mapping。
- Web Shell API types。

Gateway 仍是公开入口，负责 JWT authentication 和 admin/system.admin authorization，然后调用内部 Rust installer service。Installer service 不暴露到 host network。

## 未来拆分触发条件

满足以下一个或多个条件时，可以重新评估独立仓库：

- Installer 被多个 OJOS deployments 或外部系统复用。
- Module package format 稳定。
- Installer CLI 需要独立 release。
- Installer service 需要独立 version lifecycle。
- 主仓库 CI 明显被 Installer build/test 成本拖慢。
- Manifest schema 与 installer API 需要 formal version pinning。

## v0 边界

v0 支持本地 manifests 和本地 `.ojosmod` packages，不支持 remote marketplace、untrusted remote install、dynamic frontend bundles 或 executable install hooks。

v0 执行 checksum integrity verification。Schema 中保留 signature fields，但 signature validation 和 trust policy 延后。

Kernel modules 与 `ojos.judge-core` 受 disable/uninstall apply 保护。Demo module 与 sample module 可用于 lifecycle acceptance。
