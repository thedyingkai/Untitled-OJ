# Kernel Installer

> 文档状态：当前实现，Rust installer moved to Kernel
> 适用范围：Installer 开发 / 部署 / 安全审计
> 最后更新：2026-06-27

Installer 是 OJOS Kernel 能力，不是普通业务服务。

## Canonical Source Paths

```text
kernel/installer/core
kernel/installer/service
kernel/installer/cli
```

`kernel/installer/core` 包含 manifest/package/plan/dependency 纯逻辑，不依赖 Gateway 或 frontend。`kernel/installer/service` 是 internal HTTP service，通过 DB 和 internal API 与系统交互。`kernel/installer/cli` 提供 `ojosctl`。

## Adapter Boundary

- Gateway 是 admin HTTP adapter。
- Web Shell 是 UI adapter。
- Compose 是 runtime deployment adapter。
- Installer Core 不依赖这些 adapter。

## Safety Boundary

v0 不支持远程市场，不执行 hook，不加载 dynamic frontend bundle。`.ojosmod` v0 只做 checksum integrity，signature/trust policy 留到 v1。
