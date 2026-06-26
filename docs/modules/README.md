# 模块系统

> 文档状态：部分实现
> 适用范围：架构设计 / 模块开发规划 / 管理后台
> 最后更新：2026-06-26

## 1. 文档目的

本文档是 OJOS 模块系统入口，用于说明 `Service -> Module -> Set -> OJOS` 的层级关系，以及当前已经完成的 Module Registry v0 与后续 installer v0 的边界。

当前阶段已经把现有 A/Judge Core 能力登记为 builtin module，并提供只读模块拓扑 API 和管理后台页面。本文档不能被理解为完整热插拔平台已经完成：安装、启用、禁用、升级、回滚、远程模块市场和 B Contest 主体开发都尚未开始。

## 2. 适用范围

本文档适用于三类读者：

- 平台开发者：理解模块表结构、manifest、权限点和前端路由如何被登记。
- 管理后台开发者：维护 `/admin/modules`、`/admin/modules/topology` 和 `/admin/modules/:id` 页面。
- 后续模块开发者：在实现 installer v0 或 B Contest 前确认边界，避免把 Contest 特性提前写入 Core。

## 3. 当前实现

当前仓库已经实现 Module Registry v0，只读展示能力落在 Gateway 管理域：

| 能力 | 当前路径 | 状态 |
| --- | --- | --- |
| 模块注册表迁移 | `deploy/migrations/000009_module_registry.up.sql` | 当前实现 |
| builtin bootstrap | `services/gateway/internal/moduleregistry` | 当前实现 |
| Judge Core manifest | `modules/judge-core/module.yaml` | 当前实现 |
| 管理 API | `GET /api/admin/modules*` | 当前实现 |
| 前端模块列表 | `frontend/src/views/admin/AdminModulesView.vue` | 当前实现 |
| 前端拓扑表格 | `frontend/src/views/admin/AdminModuleTopologyView.vue` | 当前实现 |
| 前端模块详情 | `frontend/src/views/admin/AdminModuleDetailView.vue` | 当前实现 |
| installer v0 | 尚未实现 | 目标架构 |
| B Contest | 尚未开始 | 目标架构 |

Gateway 启动时会幂等登记 Kernel 内置模块和 `ojos.judge-core`。这些模块写入 `module_sets`、`module_nodes`、`module_edges`、`module_components`、`module_installations`、`module_permissions`、`module_menus`、`module_frontend_routes`、`module_gateway_routes` 和 `module_migrations`。PostgreSQL 是事实源；当前阶段没有远程模块市场，也不会从外部下载模块。

已登记的 Kernel 模块包括：

- `ojos.kernel.edge-ui-shell`
- `ojos.kernel.identity-access`
- `ojos.kernel.module-runtime`
- `ojos.kernel.config-secret`
- `ojos.kernel.audit-policy`

已登记的 Core 模块：

- `ojos.judge-core`

## 4. 目标设计

目标架构仍然是 Kernel + Core + 可安装模块。模块按 Set 组织：

- Kernel Set：身份、权限、配置、审计、模块运行时、统一前端 shell。
- Core Capability Set：题目、提交、评测、Worker Link、结果查询、评测集群管理。
- Competition Set：后续 B Contest 模块，不能在本阶段开发主体功能。
- Education Set、Collaboration Set、Integration Set、Operations Set：后续扩展集合。

完整 installer v0 应支持 `validate / install / enable / disable`，并在失败时提供可审计状态和回滚边界。当前只读 Module Registry v0 为 installer v0 准备数据模型和展示面，不执行任何安装副作用。

## 5. 关键流程

当前只读流程如下：

```mermaid
flowchart LR
    Migration[000009 module registry migration] --> Bootstrap[Gateway bootstrap builtin modules]
    Bootstrap --> DB[(PostgreSQL module_* tables)]
    DB --> API[Gateway /api/admin/modules]
    API --> UI[/admin/modules and topology]
```

模块层级如下：

```text
Service -> Module -> Set -> OJOS
```

`Service` 是实际进程、前端页面组或基础组件；`Module` 是一组可被声明和审计的能力；`Set` 是模块集合；`OJOS` 是完整平台。

## 6. 配置说明

Module Registry v0 不需要独立服务。它复用 Gateway 的数据库连接、JWT 校验和 admin 权限校验。相关配置仍在 `services/gateway/etc/gateway.yaml` 和 `.env.example` 管理。

`modules/judge-core/module.yaml` 只声明真实存在的路径、权限和路由，不携带生产 secret。worker token、JWT secret、内部 HMAC key 仍必须从环境变量或部署 secret 注入，不能写入 manifest。

## 7. 安全边界

模块系统不能绕过现有安全边界：

- 管理 API 必须由后端校验 `admin` / `super_admin` 角色或 `system.admin` 权限。
- Public API 不允许因为模块 manifest 返回内部路径字段。
- Worker 仍然只能通过 Worker Link 工作，不直连 PostgreSQL/Redis，不挂载 Control Plane storage。
- `module.yaml` 只是声明文件，不代表 route 自动公开。
- `system.admin` 属于 Kernel 身份权限模块，Judge Core 后台菜单可以要求它，但 Judge Core 不把它声明为自身业务权限。

## 8. 验收方式

当前阶段验收方式：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

并分别执行 Go、Rust、前端构建测试。运行时可在完成迁移并启动 Gateway 后访问：

```http
GET /api/admin/modules
GET /api/admin/modules/sets
GET /api/admin/modules/topology
GET /api/admin/modules/ojos.judge-core
```

预期结果是普通用户访问返回 403，管理员能看到 Kernel 模块和 `ojos.judge-core`。本阶段不验证安装、禁用、升级和回滚，因为 installer v0 尚未实现。

## 9. 常见问题

- 页面能看到模块是否表示可安装：不是。当前模块是 builtin registration，不是 installer 安装结果。
- 为什么不直接开发 B Contest：Contest 是第一个热插拔验证模块，应等 installer v0 的 validate/install/enable/disable 能力具备后再开始。
- 为什么 `system.admin` 不在 Judge Core 权限列表中：它属于 Kernel 身份权限边界；Judge Core 的后台页面只是引用该权限作为访问条件。
- 为什么拓扑先用表格展示：当前目标是数据真实、可验收、稳定，图形化拓扑可以在后续引入 Vue Flow、AntV X6 或 Cytoscape。

## 10. 相关文档

- [模块契约](module-contract.md)
- [模块清单](module-manifest.md)
- [模块生命周期](module-lifecycle.md)
- [模块安装器](module-installer.md)
- [Judge Core 模块](judge-core.md)
- [模块拓扑设计](../architecture/module-topology.md)
- [Admin API](../api/admin-api.md)
