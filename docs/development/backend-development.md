# 后端开发

> 文档状态：当前实现
> 适用范围：后端开发 / API 维护 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS 后端服务的开发约束、构建方式、权限边界和常见排查方法。后端由多个 go-zero 服务和共享 Go 模块组成，必须保持 API、权限和内部鉴权一致。

## 2. 适用范围

适用于维护 `services/gateway`、`services/auth`、`services/problem-api`、`services/judge-api` 和 `services/shared` 的开发者。

## 3. 当前实现

后端服务：

- `services/gateway`：公开入口、JWT 校验、内部 HMAC 签名。
- `services/auth`：登录、注册、当前用户、权限管理。
- `services/problem-api`：题目 CRUD 和题目包校验。
- `services/judge-api`：提交、结果、Worker Link、admin judge。
- `services/shared`：JWT、权限、HMAC、日志和数据库公共工具。

## 4. 目标设计

每个 API 都应有明确权限、错误处理和日志上下文。Public API 不返回内部路径；admin API 必须后端校验；Worker API 必须校验 worker token 和 task lease。

## 5. 关键流程

浏览器请求进入 Gateway。Gateway 校验 JWT，将用户上下文转发给内部服务，并用 HMAC 签名。内部服务验证 HMAC 后执行权限检查和业务逻辑。Judge worker 请求也经过 Gateway，但由 Judge API 额外校验 `X-OJOS-Worker-Token`。

## 6. 配置说明

服务配置位于各服务 `etc/*.yaml`，运行时 secret 通过环境变量或部署配置传入。不能在代码中写死生产 DSN、secret、worker token 或 Windows 路径。

## 7. 安全边界

内部服务不对公网开放。客户端伪造 `X-Auth-Verified` 不可信。权限系统必须在后端执行。任何新增响应结构都要检查是否泄露内部路径。

## 8. 验收方式

```powershell
cd services\judge-api
go test ./...
```

全仓库静态验证会依次执行 `go build ./...`、`go test ./...` 和路径泄露扫描。

## 9. 常见问题

- HMAC 校验失败：检查 Gateway 和内部服务密钥是否一致。
- 普通用户越权：检查 handler 是否调用权限逻辑。
- worker result 被拒绝：检查 `worker_id`、`task_id` 和 `lease_version`。
- migration 缺失：检查 `deploy/migrations/` 是否包含字段变更。

## 10. 相关文档

- [服务拓扑](../architecture/service-topology.md)
- [内部 HMAC](../architecture/internal-auth.md)
- [Worker Link 协议](../architecture/worker-link-protocol.md)
