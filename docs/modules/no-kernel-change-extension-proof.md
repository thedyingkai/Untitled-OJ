# 不改 Kernel 的扩展证明

`modules/sample-hello` 是 SDK 样例模块，用于证明普通 metadata/service/route/menu/permission/topology 模块可以通过 manifest/package/runtime 接入，不需要为样例模块修改 Kernel、Gateway 或 Web Shell 主逻辑。

## 本轮新增或修改

- `modules/sample-hello/`
- `docs/modules/*` SDK、schema、testing 文档
- `scripts/e2e-module-compat.ps1`
- `ojosctl module init/package/verify` 相关能力
- schema/package/installer compatibility tests

## 未为样例模块修改的核心逻辑

- 没有为 `sample-hello` 修改 Kernel installer core 校验规则。
- 没有为 `sample-hello` 修改 Kernel installer service 的核心 install/enable/disable 逻辑。
- 没有在 Gateway 中写 sample-specific route。
- 没有在 Web Shell 主菜单硬编码 sample。
- 没有在 topology 页面硬编码 sample。
- 没有在 permission 页面硬编码 sample。

## 接入路径

1. Installer 校验并存储 `module.yaml`。
2. Module Registry 持久化 module node、permission、menu、route、component 等元数据。
3. Kernel Module Runtime 从 registry 和 stored manifest 派生 Runtime Snapshot。
4. Gateway 从 Runtime Snapshot 构建 route table。
5. Web Shell 从 Runtime Snapshot 展示 menu、contribution、topology、permission 和 runtime service。

## 仍可能需要 Kernel 演进的情况

- 新增 extension point 类型。
- 新增 service runtime driver。
- 新增 dynamic frontend bundle 能力。
- 新增 remote market trust policy。
- 新增 hook execution。
- 新增 package signing policy。
- 实现完整模块热插拔自动化。

普通 metadata/service/route/menu/permission/topology 模块在 schema v1 内不需要改 Kernel/Gateway/Web Shell 主逻辑。
