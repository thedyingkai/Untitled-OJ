# 本地开发

> 文档状态：当前实现
> 适用范围：开发 / 本地调试 / 静态验收
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS 在本地开发环境中的启动、构建、验证和排查方式。它帮助开发者明确哪些工作可以在 Windows 静态环境完成，哪些必须在 Docker/Linux worker 环境中完成。

## 2. 适用范围

适用于前端开发、Go 服务开发、Rust worker 静态检查、文档重构和部署配置调整。真实 nsjail/cgroup、多 worker 并发和资源限制不属于普通 Windows 本地开发能力。

## 3. 当前实现

仓库包含 `frontend`、`services/*`、`deploy/compose`、`deploy/worker` 和 `scripts`。本地静态验证入口为 `scripts/verify-static.ps1`，前端开发入口为 `npm run dev`。

## 4. 目标设计

本地开发应尽量用统一脚本发现问题，避免每个模块各自维护临时命令。新增模块后，应接入静态验证和文档索引。

## 5. 关键流程

先执行静态验证，再启动前端或服务进行局部调试：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
cd frontend
npm run dev
```

## 6. 配置说明

前端使用 `frontend/.env` 中的 `VITE_API_BASE_URL`。后端和 compose 使用 `.env`，可从 `.env.example` 复制并替换 secret 占位值。

## 7. 安全边界

本地 `.env` 不应提交。临时日志、扫描报告和调试脚本放入 `.tmp/agent/`，不能放在根目录或正式源码目录。

## 8. 验收方式

本地修改完成后至少执行 `verify-static.ps1 -SkipDockerBuild`。前端页面修改需确认 `npm run build` 通过。

## 9. 常见问题

- 端口冲突：检查本地服务或 Vite dev server。
- API 401：检查 token、auth store 和 `VITE_API_BASE_URL`。
- Go 包找不到：确认在对应服务目录执行，或使用静态验证脚本。
- Docker 不可用：只影响镜像构建和运行验收，不影响静态检查。

## 10. 相关文档

- [静态验证](static-verification.md)
- [前端开发](frontend-development.md)
- [临时文件隔离规则](temp-file-policy.md)
