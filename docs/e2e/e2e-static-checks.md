# E2E 静态检查

> 文档状态：当前实现
> 适用范围：开发 / E2E 验收 / 发布前检查
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明在 Windows 或无 Linux sandbox 环境中可以执行的静态验收。静态检查用于尽早发现格式、构建、类型、安全扫描和 compose 配置问题，但不能证明 nsjail、cgroup v2 或多 worker runtime 行为通过。

## 2. 适用范围

适用于日常开发、文档重构、提交前检查和 CI 的基础阶段。任何涉及 Docker image build 或 Linux worker runtime 的结论，都必须在对应环境中另行记录。

## 3. 当前实现

静态验收脚本位于 `scripts/verify-static.ps1`。使用 `-SkipDockerBuild` 时会跳过镜像构建，但仍执行 Docker compose config。

## 4. 目标设计

静态验收应保持快速、确定、可在普通开发机执行。新增 public API、前端页面、worker 协议或部署文件时，应同步扩展扫描规则，避免路径泄露和危险部署配置回归。

## 5. 关键流程

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

检查内容：

- `gofmt`。
- Go build/test。
- `cargo fmt --check`。
- `cargo check`。
- `npm run build`。
- `docker compose config`。
- 前端直接 API 调用和 mock 扫描。
- Public schema 内部路径扫描。
- 危险部署配置扫描。

## 6. 配置说明

脚本从仓库根目录执行。需要安装 `go`、`cargo`、`npm`、`docker` 和 `rg`。使用 `-SkipDockerBuild` 时不要求 Docker daemon 能构建镜像，但 `docker compose config` 仍需要 Docker CLI 可用。

## 7. 安全边界

静态扫描会阻止 public schema 泄露内部路径、前端绕过统一 API client、部署文件出现危险配置。它不检查生产网络策略和真实 worker sandbox。

## 8. 验收方式

脚本退出码为 0 才能记录为通过。失败时查看输出的阶段名，例如 `Frontend build` 或 `Dangerous deployment scan`，修复后重新执行完整命令。

## 9. 常见问题

- `npm run build` 失败：进入 `frontend` 单独执行并修复 TypeScript/Vite 错误。
- compose config 失败：检查 `.env.example` 和 compose 文件。
- public schema scan 失败：移除 API schema 中的内部路径字段。
- frontend direct-call scan 失败：改为统一 API client。

## 10. 相关文档

- [工程验收总入口](e2e-engineering-acceptance.md)
- [静态验证](../development/static-verification.md)
- [临时文件隔离规则](../development/temp-file-policy.md)
