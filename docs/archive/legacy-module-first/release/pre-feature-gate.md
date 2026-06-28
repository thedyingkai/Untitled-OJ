# 功能实现前门禁

本文定义 v0.1.0 发布基线之后、开始第一个真实业务模块之前必须满足的门禁。当前暂停 Contest Core Skeleton，本门禁不允许写 Contest API、Contest 前端或 Contest migration。

## 必须通过的检查

- `scripts/acceptance-kernel.ps1 -SkipDockerBuild` 通过。
- `scripts/acceptance-kernel.ps1 -RunControlledApply -SkipDockerBuild` 通过，或明确记录 controlled apply 未运行且本轮不声明 apply 通过。
- `scripts/verify-static.ps1 -SkipDockerBuild` 通过。
- `scripts/e2e-api.ps1` 返回 `failed=0`、`path_leaks=0`、`admin_health_status=ok`、`admin_health_judge_status=ok`。
- `scripts/e2e-module-compat.ps1` 返回 `failed=0`、`sample_module_compat=passed`。
- Go 多模块测试通过。
- Rust root `cargo fmt --check`、`cargo check`、`cargo test` 通过。
- Judge Worker `cargo fmt --check`、`cargo check`、`cargo test` 通过。
- Frontend `npm audit --audit-level=high` 无 high vulnerability，`npm run build` 通过。
- release artifact 构建成功，产物仅写入 `.tmp/release/<version>/`。

## 契约门禁

- 所有 checked-in module manifest 使用 `schema_version: 1`。
- Module Contract v1 继续拒绝 unknown top-level fields。
- dangerous fields 继续拒绝：`command`、`script`、`hook`、`image`、`mount`、`host_path`、`privileged`、`cap_add`、`target_url`、secret/token/password-like 字段。
- `.ojosmod` package format 保持 `version: 1`，只声明 checksum integrity，不声明 publisher trust 已完成。
- Runtime Snapshot 保持 `version: 1`。

## 安全门禁

- Gateway/Web 不执行 runtime apply。
- Gateway/Web/module-installer 不挂载 Docker socket。
- Web Shell 的 Installer 页面只作为管理视图，不作为官方安装器主入口。
- 官方安装、打包、验证、启用、禁用和 runtime apply 入口是 `ojosctl` / `ojos-installer-tui`。
- Dynamic Gateway proxy 只接受可信 `service_id`，不接受 manifest 提供的 URL。
- Reserved prefix 继续受保护。
- 原始 `Authorization` 不透传到模块服务。
- `path_leaks=0`。
- 无真实 secret、本机绝对路径或构建垃圾进入 Git。

## 下一阶段准入

只有当验收、契约、安全和文档门禁全部通过时，才建议进入真实业务模块设计或 skeleton 实现。第一个真实模块必须限制在 Module Contract v1 范围内；若需要新增 extension point、runtime driver 或 dynamic frontend bundle，必须先做 Kernel 设计评审。

## 仍然禁止

- 无计划开始 Contest。
- 写 Contest API、Contest 前端或 Contest migration。
- 做 remote module market。
- 执行 hook。
- 动态加载不可信 JS。
- 把 Judge Core 标记 GA。
- 宣称完整模块热插拔自动化已经完成。
