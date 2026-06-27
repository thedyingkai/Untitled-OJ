# Module Installer

> 文档状态：当前实现，v0 本地 manifest / 本地 package 验收中
> 适用范围：模块开发 / 后端开发 / 运维 / 安全审计
> 最后更新：2026-06-27

## 设计目标

Module Installer 是 OJOS 的模块生命周期基座。它不是一个简单的管理按钮，而是负责把模块声明从 `module.yaml` 和 `.ojosmod` 包转换为可审计、可回滚规划、可权限控制的系统状态变更。

当前实现采用 Rust：

```text
kernel/installer/core/     纯逻辑核心
kernel/installer/service/  内部 HTTP service
kernel/installer/cli/      本地 CLI
```

仓库边界见 [Project Structure v2 ADR](../architecture/adr/ADR-project-structure-v2-kernel-modules.md) 和 [ADR: Module Installer Repository Boundary](../architecture/adr/ADR-module-installer-repository-boundary.md)。当前不立即拆仓，但 Installer 已从普通 service 提升为 Kernel 能力。

## 当前 v0 能力

v0 已实现：

- 本地 `modules/*/module.yaml` discover / validate / plan。
- 本地 `.ojosmod` package / verify / inspect。
- manifest schema version 1 校验。
- 路径安全校验、危险字段校验、重复声明校验。
- 依赖解析与 install / enable / disable / upgrade / rollback / uninstall plan。
- demo module 的 install apply / enable / disable。
- operation lock、operation history、request/result redaction 和 audit log。
- Gateway Admin API 接入。
- 前端 `/admin/modules/installer` 管理页。

v0 明确不支持：

- 远程模块市场。
- 远程不可信模块自动安装。
- 动态 frontend bundle 执行。
- install hook / postinstall / preinstall 脚本执行。
- judge-core / kernel 的 disable 或 uninstall apply。
- 真实跨仓库独立发布。

## 内部服务

`module-installer` 只在 compose internal network 中暴露：

```text
GET  /health
GET  /internal/modules/discover
POST /internal/modules/validate
POST /internal/modules/plan
POST /internal/modules/install
POST /internal/modules/:id/enable
POST /internal/modules/:id/disable
POST /internal/modules/:id/upgrade-plan
POST /internal/modules/:id/rollback-plan
POST /internal/modules/:id/uninstall-dry-run
GET  /internal/modules/:id/health
GET  /internal/modules/:id/operations
```

内部调用必须携带 `X-OJOS-Installer-Token`。该 token 由 Gateway 注入，不能暴露给前端或用户。

## Gateway API

外部只通过 Gateway：

```text
GET  /api/admin/modules/discover
POST /api/admin/modules/validate
POST /api/admin/modules/plan
POST /api/admin/modules/install
POST /api/admin/modules/:id/enable
POST /api/admin/modules/:id/disable
POST /api/admin/modules/:id/upgrade-plan
POST /api/admin/modules/:id/rollback-plan
POST /api/admin/modules/:id/uninstall-dry-run
GET  /api/admin/modules/:id/health
GET  /api/admin/modules/:id/operations
```

Gateway 负责 JWT 鉴权、`admin` / `super_admin` / `system.admin` 权限检查、actor 信息透传和错误映射。前端不直接访问 installer service。

## Runtime Image Hardening

`kernel/installer/service` 使用多阶段 Dockerfile：

```text
builder: rust:1.89-bookworm
runtime: debian:bookworm-slim
```

最终 runtime image 只复制 `module-installer` binary 和 CA bundle，不包含 cargo、rustc 或源码。Compose 中 `module-installer` 只通过 internal network `expose: 8090` 暴露，不发布宿主机端口，不挂载 Docker socket，不挂载 `.env`，只读挂载 `modules/`。

当前 compose hardening：

```text
read_only: true
security_opt: no-new-privileges:true
cap_drop: ALL
tmpfs: /tmp
USER 65532:65532
```

后续目标是评估 `gcr.io/distroless/cc-debian12` 等 distroless runtime；v0 hardening 先使用 `debian:bookworm-slim` 以保留证书和动态链接调试余地。

## Error Model

Rust internal API 错误统一返回：

```json
{
  "error": {
    "code": "MANIFEST_PATH_ESCAPE",
    "message": "manifest path escapes modules directory",
    "severity": "error",
    "details": {}
  }
}
```

Gateway 会把 manifest/path/validation error 映射为 400，未登录映射为 401，权限不足映射为 403，模块不存在映射为 404，operation lock 或 dependency conflict 映射为 409，installer 不可达映射为 503。错误响应不得泄露 internal service URL、DB 连接串、Rust panic、SQL 错误或绝对路径。

## Operation Lock

写操作使用 `module_operation_locks` 全局锁，TTL 默认 300 秒，可通过 `MODULE_INSTALLER_LOCK_TTL_SECONDS` 配置，允许范围为 30 到 3600 秒。install / enable / disable / upgrade / rollback / uninstall apply 都必须持锁。

dry-run 默认不写业务表。apply 操作会写入 `module_operations`，并在 `permission_audit_logs` 里记录 `module.<action>`。operation request/result 写入前会 redaction，`token`、`secret`、`password`、`authorization` 字段不会原样保存。

## 保护规则

- kernel 模块不可 disable。
- `ojos.judge-core` 默认不可 disable。
- kernel / builtin / judge-core 不可 uninstall apply。
- 有 enabled dependent 时，不允许 disable / uninstall。
- uninstall apply v0 仅为 demo module 预留；涉及业务数据的模块默认只能 dry-run。

## 验收方式

本地 CLI：

```powershell
cargo run -p ojosctl -- module validate modules/demo-module/module.yaml --repo-root .
cargo run -p ojosctl -- module plan modules/demo-module/module.yaml --repo-root .
cargo run -p ojosctl -- module package modules/demo-module -o .tmp/agent/scratch/demo.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/demo.ojosmod
cargo run -p ojosctl -- module inspect .tmp/agent/scratch/demo.ojosmod
cargo run -p ojosctl -- module doctor
```

运行时 API：

```powershell
powershell -NoProfile -File scripts\e2e-api.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123 `
  -WorkerToken $env:OJOS_WORKER_TOKEN
```
## 安全参考

更多攻击面、缓解措施和剩余边界见 [Module Installer Threat Model](../security/module-installer-threat-model.md)。

## Runtime Wiring v1 Installer Notes

Installer v0 writes metadata used by Kernel Module Runtime. Module enable/disable directly affects active Runtime Snapshot membership. Disabled module registry records are retained for audit, detail views and include-disabled inspection.

The demo module intentionally declares disabled menu, frontend route and gateway route metadata. This validates the registry/runtime contribution path without pretending a real business API or frontend bundle exists.

Gateway route declarations are validated as metadata in v1. The route table API can aggregate and detect conflicts, but full dynamic proxy cutover is future work.
