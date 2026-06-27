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

## Runtime Wiring Boundary

Kernel Installer writes module registry data, and Kernel Module Runtime reads registry tables plus stored manifest metadata to build Runtime Snapshot. The installer does not need Gateway or Web Shell dependencies. Gateway only exposes admin adapter APIs and forwards installer operations to the internal Rust service.

`gateway_routes.auth_mode` supports `public`, `user`, `admin`, `worker`, and `internal` as the forward-facing route contract. Compatibility aliases `none`, `optional`, and `required` are still accepted for existing Judge Core routes and normalize to public/user semantics in the runtime route table.

## Hotplug L1 Route Contract

Installer Core accepts `gateway_routes.service_id` as the forward contract and keeps `target_service` as a compatibility alias. It rejects direct `target_url` through deny-unknown-fields and dangerous field checks. The installer never resolves upstream URLs; Gateway owns trusted service resolution.
## Hotplug L2 Runtime Commands

`ojosctl` now includes local runtime inspection commands:

```powershell
cargo run -p ojosctl -- runtime services
cargo run -p ojosctl -- runtime service problem-api
cargo run -p ojosctl -- runtime plan-restart problem-api
```

These commands read local module manifests and generate JSON output. They are plan-only in L2 foundation. `runtime apply-plan` intentionally returns non-zero / not implemented until a controlled operator path exists.
