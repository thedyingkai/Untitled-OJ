# 静态验证

> 文档状态：当前实现
> 适用范围：开发 / 发布前检查 / E2E 静态验收
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 `scripts/verify-static.ps1` 的检查范围、执行方式和失败处理。它是 OJOS 当前最重要的本机验收入口。

## 2. 适用范围

适用于代码修改、文档重构、部署配置调整和提交前检查。它不替代 Docker runtime、Linux cgroup v2、nsjail 或多机 worker 验收。

## 3. 当前实现

脚本位于 `scripts/verify-static.ps1`，已经包含中文文件头说明。它会执行 Go、Rust、前端和部署配置检查，并扫描危险内容。

## 4. 目标设计

静态验证应覆盖所有无需外部运行环境即可发现的问题。新增 API schema、前端路径、部署配置或 worker 协议时，应同步更新扫描规则。

## 5. 关键流程

跳过 Docker build：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

完整静态验证：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1
```

完整模式需要 Docker daemon，因为会执行镜像构建。

脚本目前按仓库的多语言结构执行检查：Go 服务是多个独立 module，因此脚本会分别进入 `services/shared`、`services/auth`、`services/gateway`、`services/problem-api`、`services/judge-api` 执行构建和测试；Rust 检查在 `services/judge-worker` 内执行；前端构建在 `frontend` 内执行。不要把根目录 `go test ./...` 当作当前仓库的唯一验收命令，因为根目录不是 Go module。

## 6. 配置说明

脚本从仓库根目录执行，依赖 `go`、`cargo`、`npm`、`docker` 和 `rg`。Docker daemon 不可用时，使用 `-SkipDockerBuild`。

## 7. 安全边界

脚本会扫描 public schema 内部路径、前端直接 API 调用、mock、危险部署配置和 docs/archive 之外的危险内容。它不判断生产 secret 是否已按实际环境轮换。

## 8. 验收方式

脚本所有阶段通过并输出 `Static verification completed.` 才算通过。任一阶段失败都必须修复后重跑。

通过后仍需区分静态验收和运行验收：静态验收能发现格式、构建、schema、危险配置和前端构建问题；它不能证明 Docker 镜像已构建、nsjail/cgroup 真实生效、第二台 worker 已接入，也不能证明多机恢复已经通过。

## 9. 常见问题

- Go fmt 失败：执行 `gofmt -w` 修复相关文件。
- Rust check 失败：进入 `services/judge-worker` 执行 `cargo check`。
- 前端构建失败：进入 `frontend` 执行 `npm run build`。
- Docker compose config 失败：检查 `.env.example` 与 compose 变量。

## 10. 相关文档

- [E2E 静态检查](../e2e/e2e-static-checks.md)
- [工程验收总入口](../e2e/e2e-engineering-acceptance.md)
