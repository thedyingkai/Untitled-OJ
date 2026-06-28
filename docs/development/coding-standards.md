# 编码规范

> 文档状态：当前实现
> 适用范围：开发 / 代码评审 / 质量控制
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义 OJOS 代码和文档维护的基础规范，减少临时实现、权限绕过、路径泄露和不可验收改动。

## 2. 适用范围

适用于 Go 后端、Rust worker、Vue 前端、部署配置、脚本和文档修改。

## 3. 当前实现

项目使用 Go/go-zero、Rust、Vue 3 + Vite + TypeScript。静态验证脚本会检查 Go 格式、Go 测试、Rust 格式、Rust check、前端构建和危险内容。

## 4. 目标设计

所有模块应具备清晰配置、错误处理、权限校验、日志上下文、健康检查和可执行验收。文档必须用中文说明工程边界。

## 5. 关键流程

修改前先阅读相邻代码；修改后执行对应模块测试；最后执行 `scripts/verify-static.ps1 -SkipDockerBuild`。

## 6. 配置说明

生产 secret、DSN、worker token 不写入代码和文档。配置通过环境变量、`.env.example` 和服务配置模板表达。

## 7. 安全边界

Public API 不返回内部路径；admin API 必须后端权限校验；Worker API 必须校验 token 和 lease；前端 guard 不是安全边界。

## 8. 验收方式

Go 使用 `gofmt` 和 `go test ./...`；Rust 使用 `cargo fmt --check` 和 `cargo check`；前端使用 `npm run build`。

## 9. 常见问题

- 为了赶进度写 mock：不允许进入正式页面。
- 只在前端隐藏按钮：不构成权限控制。
- 日志缺上下文：补 request_id、submission_id 或 worker_id。
- 临时脚本放入 `scripts/`：应放入 `.tmp/agent/scripts/`。

## 10. 相关文档

- [后端开发](backend-development.md)
- [前端开发](frontend-development.md)
- [临时文件隔离规则](workspace-file-policy.md)
