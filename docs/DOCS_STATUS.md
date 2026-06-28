# OJOS 文档状态

日期：2026-06-28

## v0.1.0 状态

OJOS 正在进行 `v0.1.0 Release Hardening`。当前目标是发布题库、评测、Judge Core、Kernel、Module Runtime、Installer 和 Module SDK 的可验收基线。

正式安装入口为 `ojosctl` 和 `ojos-installer-tui`。Web Shell 中的 Installer 页面只作为管理视图，不作为官方安装器主入口。

## Feature Planning 状态

Feature Module Planning Gate v1 已完成，推荐未来第一个真实业务模块为 Contest Core Skeleton。但当前暂停 Contest Core Skeleton，本轮不实现 Contest API、Contest 前端或 Contest migration。

## Hotplug 状态

| 等级 | 状态 | 说明 |
| --- | --- | --- |
| L0 Metadata Hotplug | 完成 | Registry、Runtime Snapshot、permissions、menus、topology metadata 和 health metadata |
| L1 Route/Menu/Topology/Permission Hotplug | 基本完成 | Trusted dynamic route table/proxy 和 Web Shell contribution registry |
| L2 Service Runtime Foundation + Controlled Apply | foundation 完成 | Services/workers、route-health linkage、plans 和 `ojosctl` controlled apply |
| L3 Dynamic Frontend Extension | 未完成 | 不加载不可信 dynamic JS 或 frontend bundle |
| L4 Full Module Hotplug | 未完成 | 无 remote market、hooks 或完整 service automation |

## 必须通过的门禁

Pre-feature gate 要求：

- `scripts/acceptance-kernel.ps1 -SkipDockerBuild`
- `scripts/verify-static.ps1 -SkipDockerBuild`
- `scripts/e2e-api.ps1`
- `scripts/e2e-module-compat.ps1`
- Go `go test ./...`
- Rust root `cargo fmt --check`, `cargo check`, `cargo test`
- Judge worker `cargo fmt --check`, `cargo check`, `cargo test`
- Frontend `npm run build`

要求结果：

- `failed=0`
- `path_leaks=0`
- `admin_health_status=ok`
- `admin_health_judge_status=ok`
- `sample_module_compat=passed`
- ordinary user `403`
- no token `401`

## 契约状态

| 契约 | 状态 |
| --- | --- |
| Module manifest schema v1 | 当前兼容起点 |
| Runtime Snapshot v1 | 当前模块贡献事实源 |
| `.ojosmod` package format v1 | 仅 checksum integrity |
| Package signature/trust policy | 未完成 |

## Judge Core Status

Judge Core 是当前第一个核心 feature module，已经通过 Runtime Snapshot、routes、services 和 topology 展示。Judge Core disable/uninstall 继续受保护。Judge Core 不标记 GA；真实多机、网络故障恢复、时钟漂移和长时间 soak test 仍未完成。

## 明确不在当前范围

- B Contest implementation.
- Contest API.
- Contest frontend.
- Contest migration.
- Remote module market.
- Hook execution.
- Dynamic untrusted frontend JavaScript.
- Full hotplug automation.
- Judge Core GA.
