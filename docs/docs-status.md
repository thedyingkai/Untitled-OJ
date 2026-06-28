# OJOS 文档状态

日期：2026-06-28

## 当前结论

OJOS 正在进行全仓逐文件人工审计与仓库清理。审计完成前，不宣称 `v0.1.0` 已达到 Release Candidate，也不建议推送、打 tag 或发布。

本文档是当前正式文档状态入口。历史过程记录保留在 `docs/archive/`，未来功能设计保留在 `docs/features/` 或 `docs/roadmap/`。

## 已具备的真实能力

- 认证、题库 API、评测 API、Judge Worker。
- Kernel Installer Core、Installer Service、`ojosctl`、`ojos-installer-tui`。
- Module Registry、Runtime Snapshot v1、Module Contract v1、`.ojosmod` package format v1。
- Gateway dynamic route table 和受信任 dynamic proxy。
- Web Shell contribution registry 和只读管理视图。
- Service Runtime Driver foundation 与 `ojosctl` controlled apply。
- Module SDK、`modules/sample-hello` compatibility harness。

## 当前未完成或不得宣称完成

- Contest 未实现。
- Contest Core Skeleton 未开始。
- L3 dynamic frontend bundle 未完成。
- hooks 未实现。
- remote module market 未实现。
- package signature / trust policy 未完成。
- true multi-machine runtime apply 未完成。
- 完整模块热插拔自动化未完成。
- Judge Core 不标记通用可用状态。
- 生产级安全结论尚未给出。

## Hotplug 状态

| 等级 | 状态 | 说明 |
| --- | --- | --- |
| L0 Metadata Hotplug | 已实现 | Registry、Runtime Snapshot、permissions、menus、topology metadata 和 health metadata |
| L1 Route/Menu/Topology/Permission Hotplug | 基本实现 | Gateway 与 Web Shell 读取 registry/snapshot，不为普通模块硬编码贡献 |
| L2 Service Runtime Foundation + Controlled Apply | foundation 已实现 | Services/workers、route-health linkage、plans 和 `ojosctl` controlled apply |
| L3 Dynamic Frontend Extension | 未完成 | 不加载不可信 dynamic JS 或 frontend bundle |
| L4 完整模块热插拔 | 未完成 | 无 remote market、hooks 或完整 service automation |

## 当前审计门禁

审计完成前必须重新执行并通过：

- `scripts/acceptance-kernel.ps1 -SkipDockerBuild`
- `scripts/acceptance-kernel.ps1 -RunControlledApply -SkipDockerBuild`
- `scripts/verify-static.ps1 -SkipDockerBuild`
- `scripts/e2e-api.ps1`
- `scripts/e2e-module-compat.ps1`
- Go 多模块测试。
- Rust root `cargo fmt --check`、`cargo check`、`cargo test`。
- Judge worker `cargo fmt --check`、`cargo check`、`cargo test`。
- Frontend `npm audit --audit-level=high` 和 `npm run build`。
- `scripts/build-release-artifacts.ps1 -Version v0.1.0`。

要求结果：

- `failed=0`
- `path_leaks=0`
- `admin_health_status=ok`
- `admin_health_judge_status=ok`
- `sample_module_compat=passed`
- 普通用户 `403`
- 无 token `401`
- `gateway` running
- `module-installer` healthy
- npm audit high vulnerabilities 为 `0`

## 文档维护规则

- 正式文档必须使用中文，允许保留 API path、JSON field、命令、协议名、第三方工具名和代码标识符。
- 正式文档只描述当前真实状态。
- 历史过程写入 `docs/archive/`。
- 未来规划写入 `docs/features/` 或 `docs/roadmap/`。
- 已知限制集中写入本文档和 release known limitations。
- 正式文档文件名使用小写英文、数字和连字符。

## 相关文档

- [文档索引](docs-index.md)
- [仓库文件规则](development/workspace-file-policy.md)
- [v0.1.0 已知限制](release/v0.1.0-known-limitations.md)
- [Kernel 基线冻结](release/kernel-baseline-freeze.md)
- [模块系统](modules/index.md)
