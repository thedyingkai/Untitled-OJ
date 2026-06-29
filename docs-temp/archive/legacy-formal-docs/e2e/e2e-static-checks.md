# E2E 静态检查

> 文档状态：当前实现
> 适用范围：开发 / E2E 验收 / 发布前检查
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明在 Windows 或无 Linux sandbox 环境中可以执行的静态验收。静态检查用于尽早发现格式、构建、类型和 compose 配置问题，但不能证明 nsjail、cgroup v2、多 worker runtime 行为或安全边界已经通过。

## 2. 适用范围

适用于日常开发、文档重构、提交前检查和 CI 的基础阶段。任何涉及 Docker image build 或 Linux worker runtime 的结论，都必须在对应环境中另行记录。

## 3. 当前实现

静态验收脚本位于 `scripts/verify-static.ps1`。使用 `-SkipDockerBuild` 时会跳过镜像构建，但仍执行 Docker compose config。

## 4. 目标设计

静态验收应保持快速、确定、可在普通开发机执行。新增 public API、前端页面、worker 协议或部署文件时，应同步补充编译、测试、E2E 和人工审计要求，避免路径泄露和危险部署配置回归。

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
- Docker compose 配置检查。
- 关键安全边界由人工审计和 E2E 结果确认。

## 6. 配置说明

脚本从仓库根目录执行。需要安装 `go`、`cargo`、`npm`、`docker` 和 `rg`。使用 `-SkipDockerBuild` 时不要求 Docker daemon 能构建镜像，但 `docker compose config` 仍需要 Docker CLI 可用。

## 7. 安全边界

静态检查只能发现部分配置和构建问题。public schema、前端绕过统一 API client、危险部署配置、生产网络策略和真实 worker sandbox 都必须结合人工审计与运行时验收。

## 8. 验收方式

脚本退出码为 0 才能记录为通过。失败时查看输出的阶段名，例如 `Frontend build` 或 `Docker compose config`，修复后重新执行完整命令。

## 9. 常见问题

- `npm run build` 失败：进入 `frontend` 单独执行并修复 TypeScript/Vite 错误。
- compose config 失败：检查 `.env.example` 和 compose 文件。
- API 响应出现内部路径：修复 schema、handler 或 response mapper，并重新跑 E2E。
- 前端绕过统一 API client：改为统一 client 后重新构建并人工审查页面。

## 10. 相关文档

- [工程验收总入口](e2e-engineering-acceptance.md)
- [静态验证](../development/static-verification.md)
- [临时文件隔离规则](../development/workspace-file-policy.md)
## 2026-06-26 验收边界补充

`scripts\verify-static.ps1 -SkipDockerBuild` 只属于静态验证入口，不能证明 Docker Control Plane 已启动、数据库迁移已执行、Gateway 转发正常或 API 权限拒绝正确。需要 API 运行时验收时，必须先真实启动 compose：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build
powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN
```

Linux nsjail/cgroup 资源限制仍由 `scripts/e2e-linux.sh` 单独验收；多机 worker 仍需要 Linux/Docker/多机环境。
