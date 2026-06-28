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

预期 TypeScript 与 Vite 构建通过。还应人工审查页面是否存在 mock、直接 API 调用、`console.log`、演示数据或未完成页面，并在 Docker Control Plane 可用时执行 E2E。

## 9. 常见问题

- 刷新后登录丢失：检查 auth store 持久化和 `/api/auth/me`。
- admin 页面 403：检查用户 roles/permissions 和后端权限。
- 页面白屏：检查 router lazy import 和类型错误。
- 提交不更新：检查轮询停止条件和 API 错误处理。

## 10. 相关文档

- [API 文档总览](../api/index.md)
- [编码规范](coding-standards.md)
- [静态验证](static-verification.md)

## 11. OJOS UI 体系

当前前端 UI 以 OJOS 组件体系为准，新增页面优先使用 `frontend/src/components/oj/` 下的页面头、区块、工具栏、状态标签、数据表格、代码块、JSON 查看器、空状态、错误状态和加载状态组件。

状态颜色、难度颜色、worker 状态、module 状态和 health 状态统一由 `frontend/src/utils/status.ts` 提供。时间、内存、字节、百分比和列表展示统一由 `frontend/src/utils/format.ts` 提供。

前端页面必须继续接真实 Gateway API，不写演示数据，不写本地固定 ID，不绕过统一 API client。Docker Control Plane 可用时，UI 修改后应同时执行 `scripts/e2e-api.ps1`，确认 auth、problem、judge、worker、admin health、admin judge、module registry、permission 和路径泄露防护不被破坏。

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

## Runtime Wiring v1 前端指导

Web Shell 应读取 Runtime Snapshot 中的 module-provided menus 和 contribution metadata。Static Vue routes 只作为兼容入口；新的 module metadata 应进入 Module Contributions 和 Module Topology，不做页面级 hardcoding。

规则：

- 不执行 dynamic remote bundle。
- 不为 disabled 或 metadata-only frontend contribution 添加假的可点击业务路由。
- 使用 `/admin/modules/contributions` 展示通用 metadata。
- 用户权限数据可用时，使用 snapshot menus 中的 `required_permission` 判断可见性。
- 普通用户导航保留静态兼容入口，因为 Runtime Snapshot admin API 不暴露给普通用户。

## Hotplug L1 前端指导

Web Shell 使用 Runtime Snapshot menus 和 contribution metadata 展示模块表面。未知 `component_key` 必须进入 `/admin/modules/contributions/:moduleId` 并只渲染 metadata。Web Shell 不能动态 import remote module JavaScript，不能加载 untrusted bundle，也不能为 metadata-only module 创建假的业务页面。

Static Vue routes 只作为当前 Kernel/Platform/Judge Core 页面兼容入口。未来普通模块应通过 manifest/package installation 贡献 menus 和 frontend route metadata。
## Hotplug L2 前端指导

Web Shell 包含 `/admin/runtime/services`，用于查看 service 和 worker lifecycle。页面展示 service state、health、lifecycle、runtime、routes 和生成的 start/stop/restart plans。

前端边界：

- 只生成 plan；L2 foundation 不提供 Web-triggered apply control。
- 不连接 Docker、compose、module-installer internal service 或本地文件。
- 不执行 module manifest 中的 dynamic JavaScript。
- Metadata-only service 只作为 metadata 展示，并阻止 start/stop/restart。
- 未知 module frontend `component_key` 继续通过安全的 contribution metadata 页面渲染。

## Hotplug L2 Controlled Apply 前端指导

Web Shell 可以展示 runtime plans、warnings、blocked reasons 和 operation history，但不能执行 runtime plan。

允许的 UI 行为：

- Generate plan-start, plan-stop, plan-restart, and plan-reload through Gateway admin APIs.
- Display plan JSON for operator review.
- Copy or download plan JSON.
- Show an `ojosctl runtime apply-plan ... --dry-run` / `--confirm` command example.
- Show runtime operation history.

禁止的 UI 行为：

- No direct apply button that calls Docker, compose, Gateway apply, or module-installer internals.
- No Docker socket, local file, or host path access.
- 不从 module manifest 执行 dynamic JavaScript。

L2 Controlled Apply 把 apply 权限保留在浏览器之外。

## Module SDK 前端指导

Web Shell 不能 hardcode 普通 SDK sample module。它应从 Runtime Snapshot/registry APIs 渲染 module menus、frontend route metadata、contribution detail 和 topology。未知 `component_key` 继续使用 safe metadata fallback，不能动态 import JavaScript。
