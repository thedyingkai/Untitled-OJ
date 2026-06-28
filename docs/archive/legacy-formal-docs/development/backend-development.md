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

全仓库静态验证会依次执行 Go build/test、Rust check/test、前端构建、compose config 和 Installer smoke。路径泄露必须结合代码审计与 E2E 响应结果确认。

## 9. 常见问题

- HMAC 校验失败：检查 Gateway 和内部服务密钥是否一致。
- 普通用户越权：检查 handler 是否调用权限逻辑。
- worker result 被拒绝：检查 `worker_id`、`task_id` 和 `lease_version`。
- migration 缺失：检查 `deploy/migrations/` 是否包含字段变更。

## 10. 相关文档

- [服务拓扑](../architecture/service-topology.md)
- [内部 HMAC](../architecture/internal-auth.md)
- [Worker Link 协议](../architecture/worker-link-protocol.md)
# 2026-06-27 Module Installer 后端开发补充

Module Installer 使用独立 Rust workspace：

```text
kernel/installer/core/
kernel/installer/service/
kernel/installer/cli/
```

`module-installer-core` 只包含 manifest/package/plan/依赖解析等纯逻辑，不依赖 Go 服务或 frontend。`kernel/installer/service` 是内部 HTTP service，通过 PostgreSQL 写 module registry、operation lock、operation history 和 audit log。`kernel/installer/cli` 提供本地 discover / validate / plan / package / verify / inspect / doctor。

Rust installer internal API 使用稳定错误结构：

```json
{
  "error": {
    "code": "MANIFEST_INVALID",
    "message": "manifest validation failed",
    "severity": "error",
    "details": {}
  }
}
```

Gateway 调用 installer 时必须做错误映射，不能把 internal URL、SQL 错误、panic、token 或绝对路径透传给用户。operation request/result 入库前必须 redaction。

新增 Rust 验证命令：

```powershell
cargo fmt --check
cargo check
cargo test
cargo run -p ojosctl -- --version
cargo run -p ojosctl -- module doctor
```

Gateway 仍是唯一 public API 入口。新增 Admin API 必须先在 Gateway 做 JWT 和 `system.admin` 权限校验，再调用 internal installer service。

## Runtime Wiring v1 后端指导

Gateway admin module API 应使用 Kernel Module Runtime 聚合运行时事实。新增模块贡献应来自 module registry 表或已存储的 manifest metadata，不能写页面专用硬编码。

当前后端契约：

- Runtime Snapshot 默认只返回 ENABLED 模块贡献。
- `include_disabled=true` 只用于管理员检查。
- Runtime route table 校验重复前缀和重叠前缀。
- Admin Health 从 Runtime Snapshot 聚合 `health_check` metadata，并把 metadata check 标记为 registered，不伪造成真实 service probe。
- core compatibility proxy routes 在完整 dynamic proxy 切换前保留。

## Hotplug L1 后端指导

Gateway dynamic proxy 从 Kernel Runtime route table 读取 enabled module routes。Manifest 声明 `service_id`，Gateway configuration 持有 trusted upstream URL。除 core compatibility 或 reserved platform routes 外，不要为未来模块新增 hardcoded Gateway route。

Core static routes keep priority for `/api/auth` and `/api/judge/worker`. Dynamic proxy strips hop-by-hop headers, does not forward raw Authorization by default, forwards sanitized actor headers and signs internal requests. Reserved prefixes and unknown services are blocked by the route table.
## Hotplug L2 后端指导

Runtime service lifecycle 代码属于 Kernel Runtime 边界。当前兼容实现位于 `services/gateway/internal/kernel/moduleruntime`，但必须保持与业务模块无关。

Rules for backend changes:

- Use the `RuntimeDriver` interface for service list/state/plan operations.
- Do not execute arbitrary shell strings.
- Do not mount or access Docker socket from Gateway or module-installer.
- Compose driver input must be an allowlisted service identity, never arbitrary manifest image, URL, mount or command.
- Gateway admin APIs may generate plans and expose sanitized state. They must not apply dangerous host actions.
- Dynamic proxy must check runtime route health/status and enforce auth before returning unavailable service errors.

## Hotplug L2 Controlled Apply 后端指导

Gateway 只作为 plan/status adapter。不要在 Gateway 中新增运行 Docker、调用 shell、挂载 Docker socket 或 apply runtime plan 的代码路径。

后端规则：

- Runtime plan command 必须是 argv array，不能是 shell string。
- `POST /api/admin/runtime/plans/:id/apply` 必须保持 501 边界，除非独立评审过的 operator component 接管 apply。
- Plan generation 可以对外部可执行的 trusted plan 设置 `can_apply=true`，但 Gateway 必须保持 `apply_enabled=false`。
- Operation history 和 audit response 必须 redaction secret、token、DSN、host path 和 raw env。
- 任何 compose apply 实现都必须在执行前校验 service allowlist、action allowlist、固定 compose path、TTL 和 lock。

Local controlled apply 当前属于 `kernel/installer/cli`（`ojosctl runtime apply-plan`）和未来 operator，不属于 Gateway handler。

## Module SDK 后端指导

普通模块不能新增 hardcoded Gateway route 或 module-specific runtime aggregation code。新增模块应通过 `module.yaml`、installer registry writes 和 Runtime Snapshot 接入。只有引入新的 extension point 类型、新 runtime driver 或已评审的平台能力时，才允许修改 Kernel/Gateway 代码。
