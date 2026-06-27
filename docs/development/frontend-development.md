# 前端开发

> 文档状态：当前实现
> 适用范围：前端开发 / 页面验收 / 工程维护
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS 前端工程的开发约束、目录结构、API 接入方式和验收标准。前端不是演示页面集合，而是面向真实 OJ 工作流的生产构建入口。

## 2. 适用范围

适用于维护 `frontend` 下 Vue 3 应用、页面路由、Pinia store、API client、通用组件和管理后台页面的开发者。

## 3. 当前实现

前端技术栈：Vue 3、Vite、TypeScript、Naive UI、Pinia、Vue Router。关键路径：

- `frontend/src/api/client.ts`：统一 API client。
- `frontend/src/router/`：路由与权限 guard。
- `frontend/src/stores/`：登录态和用户状态。
- `frontend/src/views/`：业务页面。
- `frontend/src/components/common/`：通用组件。
- `frontend/.env.example`：前端环境变量示例。

## 4. 目标设计

所有页面必须接真实 API，具备 loading、empty、error、403、刷新、表单校验和成功/失败提示。管理页面不能只靠前端隐藏按钮，必须与后端权限校验一致。

## 5. 关键流程

用户登录后，token 由 auth store 持久化，API client 自动附带 `Authorization: Bearer <token>`。遇到 401 时清理登录态并跳转 `/login`。提交详情页对 `PENDING` 和 `JUDGING` 状态进行轮询，终态停止。

## 6. 配置说明

`VITE_API_BASE_URL` 指向 Gateway 的 `/api`。页面不得写死 `http://localhost`，不得直接使用 `fetch` 或 `axios` 绕过统一 client。

## 7. 安全边界

前端 guard 只用于体验，不是安全边界。admin 页面、problem owner 按钮、worker 管理动作都必须依赖后端权限校验。前端不展示内部路径和 secret。

## 8. 验收方式

```powershell
cd frontend
npm run build
```

预期 TypeScript 与 Vite 构建通过。还应通过静态扫描确认没有 mock、直接 API 调用、`console.log` 和占位页面。

## 9. 常见问题

- 刷新后登录丢失：检查 auth store 持久化和 `/api/auth/me`。
- admin 页面 403：检查用户 roles/permissions 和后端权限。
- 页面白屏：检查 router lazy import 和类型错误。
- 提交不更新：检查轮询停止条件和 API 错误处理。

## 10. 相关文档

- [API 文档总览](../api/README.md)
- [编码规范](coding-standards.md)
- [静态验证](static-verification.md)

## 11. OJOS UI 体系

当前前端 UI 以 OJOS 组件体系为准，新增页面优先使用 `frontend/src/components/oj/` 下的页面头、区块、工具栏、状态标签、数据表格、代码块、JSON 查看器、空状态、错误状态和加载状态组件。

状态颜色、难度颜色、worker 状态、module 状态和 health 状态统一由 `frontend/src/utils/status.ts` 提供。时间、内存、字节、百分比和列表展示统一由 `frontend/src/utils/format.ts` 提供。

前端页面必须继续接真实 Gateway API，不写演示数据，不写本地固定 ID，不绕过统一 API client。Docker Control Plane 可用时，UI 修改后应同时执行 `scripts/e2e-api.ps1`，确认 auth、problem、judge、worker、admin health、admin judge、module registry、permission 和路径泄露扫描不被破坏。

详细设计规则见 [UI 风格指南](ui-style-guide.md)。
# 2026-06-27 Module Installer 前端开发补充

前端新增 `/admin/modules/installer` 页面，用于调用真实 Gateway API：

- discover
- validate
- install dry-run
- install apply
- enable / disable
- upgrade plan
- rollback plan
- uninstall dry-run
- module health
- operation history

页面不访问本地文件系统，不连接 internal service，不使用 mock/fake 数据。危险操作必须展示影响范围并二次确认。kernel 和 `ojos.judge-core` 需要展示保护提示，不能提供可执行的禁用/卸载 apply 按钮。

## Runtime Wiring v1 Frontend Guidance

Web Shell should read Runtime Snapshot for module-provided menus and contribution metadata. Static Vue routes remain for compatibility, but new module metadata should flow into Module Contributions and Module Topology without page-specific hardcoding.

Rules:

- Do not execute dynamic remote bundles.
- Do not add fake clickable routes for disabled or metadata-only frontend contributions.
- Use `/admin/modules/contributions` for generic metadata display.
- Use `required_permission` from snapshot menus to decide visibility where user permission data is available.
- Ordinary user navigation keeps static compatibility entries because Runtime Snapshot admin API is not exposed to ordinary users.

## Hotplug L1 Frontend Guidance

Web Shell uses Runtime Snapshot menus and contribution metadata for module surfaces. Unknown `component_key` values must route to `/admin/modules/contributions/:moduleId` and render metadata only. Web Shell must not dynamically import remote module JavaScript, must not load untrusted bundles and must not create fake business pages for metadata-only modules.

Static Vue routes remain compatibility entries for current Kernel/Platform/Judge Core pages. Future ordinary modules should contribute menus and frontend route metadata through manifest/package installation.
## Hotplug L2 Frontend Guidance

Web Shell now includes `/admin/runtime/services` for service and worker lifecycle inspection. The page shows service state, health, lifecycle, runtime, routes and generated start/stop/restart plans.

Frontend boundaries:

- Generate plans only; do not provide Web-triggered apply controls in L2 foundation.
- Do not connect to Docker, compose, module-installer internals or local files.
- Do not execute dynamic JavaScript from module manifests.
- Metadata-only services should be shown as metadata and blocked from start/stop/restart.
- Unknown module frontend `component_key` values continue to render through safe contribution metadata pages.

## Hotplug L2 Controlled Apply Frontend Guidance

Web Shell may show generated runtime plans, warnings, blocked reasons, and operation history. It must not execute runtime plans.

Allowed UI behavior:

- Generate plan-start, plan-stop, plan-restart, and plan-reload through Gateway admin APIs.
- Display plan JSON for operator review.
- Copy or download plan JSON.
- Show an `ojosctl runtime apply-plan ... --dry-run` / `--confirm` command example.
- Show runtime operation history.

Forbidden UI behavior:

- No direct apply button that calls Docker, compose, Gateway apply, or module-installer internals.
- No Docker socket, local file, or host path access.
- No dynamic JavaScript execution from module manifests.

L2 Controlled Apply keeps apply authority outside the browser.
