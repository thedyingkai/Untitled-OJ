# 当前状态

> 文档状态：当前实现
> 适用范围：开发 / 架构设计 / 验收 / 项目交接
> 最后更新：2026-06-26

## 1. 文档目的

本文档记录 OJOS 当前仓库的事实状态，防止把目标架构、运行验收或历史文档误认为已经完成。

## 2. 适用范围

适用于新接手开发者、验收人员和部署人员。阅读本文后，应能知道哪些能力已实现，哪些仍需 Linux/Docker 运行验收。

## 3. 当前实现

已实现：Gateway public API proxy、Auth 登录注册和权限管理、Problem API、Judge API、Worker Link、Admin health、Admin judge、Vue 前端、Redis signal history、PostgreSQL task lease、本地 artifact storage 和 Rust worker 资源限制代码路径。

## 4. 目标设计

未实现或仅目标化的内容包括 module registry、module installer、Contest module、前端 bundle 热插拔和对象存储生产化。

## 5. 关键流程

用户通过 Gateway 登录、浏览题目、提交代码。Judge API 创建 submission 和 task。worker 通过 Worker Link claim task、下载 artifact、评测并上传 result。

## 6. 配置说明

当前配置分布在 `.env.example`、`frontend/.env.example`、`services/*/etc/*.yaml`、`deploy/compose` 和 `deploy/worker`。

## 7. 安全边界

PostgreSQL/Redis/internal API 不公开；worker 不直连 DB/Redis；Public API 不返回内部路径。

## 8. 验收方式

静态验收执行 `scripts/verify-static.ps1 -SkipDockerBuild`。运行验收执行 `scripts/e2e-linux.sh`，需要 Linux cgroup v2、nsjail 和 Docker daemon。

## 9. 常见问题

- 文档写“已完成”但没运行：应改成“需要运行验收”。
- worker 需要 DB 凭据：说明部署方式错误。
- 资源限制未验证：只能记录为外部环境阻塞。

## 10. 相关文档

- [架构总览](../architecture/overview.md)
- [文档状态](../docs-status.md)
- [E2E 工程验收](../e2e/e2e-engineering-acceptance.md)
