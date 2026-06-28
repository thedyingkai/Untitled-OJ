# Kernel 验收矩阵

| 范围 | 验收入口 | 目标状态 | 失败处理 | 是否阻塞发布 |
| --- | --- | --- | --- | --- |
| 静态验证 | `scripts/verify-static.ps1 -SkipDockerBuild` | 通过 | 修复 build/test/config/smoke 失败项 | 是 |
| Docker API e2e | `scripts/e2e-api.ps1` | `failed=0` | 查看 `.tmp/agent/reports/api-runtime` 和服务日志 | 是 |
| Module Compatibility | `scripts/e2e-module-compat.ps1` | `sample_module_compat=passed` | 查看 `.tmp/agent/reports/module-compat` | 是 |
| CLI smoke | `ojosctl doctor/status/module/runtime` | 通过 | 修复 CLI 或契约漂移 | 是 |
| Native TUI smoke | `ojos-installer-tui --version` 和 Rust build | 通过 | 修复 TUI 构建或依赖 | 是 |
| Controlled Apply | `acceptance-kernel.ps1 -RunControlledApply` | 显式通过 | 修复 allowlist、lock、history、redaction | 涉及 apply 时阻塞 |
| Release Artifacts | `scripts/build-release-artifacts.ps1 -Version v0.1.0` | 生成 manifest/checksums | 修复构建或网络依赖 | 是 |
| Path Leak | e2e summary 与人工审查 | `path_leaks=0` | 删除泄露字段或修正响应 | 是 |
| Permission Rejection | e2e 普通用户/无 token | 普通用户 `403`，无 token `401` | 修复认证/授权边界 | 是 |

`scripts/acceptance-kernel.ps1` 是本地统一入口，summary 必须包含：

```text
static_failed
api_failed
compat_failed
path_leaks
admin_health_status
admin_health_judge_status
module_compat
controlled_apply
overall_status
```

`overall_status=ok` 只表示脚本覆盖范围通过，不能替代人工安全审计和发布审查。
