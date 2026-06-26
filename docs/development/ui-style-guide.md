# OJOS UI 风格指南

> 文档状态：当前实现
> 适用范围：前端 UI / 组件设计 / 页面验收
> 最后更新：2026-06-27

## 1. 设计目标

OJOS 前端面向长期使用的在线评测平台，而不是演示型后台。UI 应兼顾刷题用户、题目维护者和系统管理员，保持清爽、克制、工程感和竞赛平台感。

本阶段参考成熟 OJ 的信息架构经验：

- 洛谷：题目难度色彩、中文 OJ 亲和力、题目信息组织。
- Codeforces：高密度提交列表、状态列、提交/题目/用户信息组织。
- AtCoder：低干扰题面阅读、清晰导航、直接提交入口。
- DOMJudge：评测队列、worker、任务 lease 和现场运维视角。

仅吸收信息架构与交互经验，不复制任何平台的商标、logo、图片、具体色板或独特视觉识别。

## 2. 视觉原则

- 背景使用浅灰白，内容承载区使用白色卡片或 section。
- 主色统一为青绿色系，成功/警告/错误/等待状态必须通过统一状态工具生成。
- 表格优先高信息密度，行 hover 清晰，列宽可预期。
- 题面阅读页控制正文宽度，减少干扰。
- 管理页允许更高密度，但仍使用同一套 token、tag、section 和 table。
- 不使用营销落地页式 hero、夸张渐变、大屏装饰或无意义卡片堆叠。

## 3. 设计 Token

全局 token 定义在 `frontend/src/style.css`：

- `--ojos-primary`
- `--ojos-success`
- `--ojos-warning`
- `--ojos-danger`
- `--ojos-muted`
- `--ojos-card-bg`
- `--ojos-border`
- `--ojos-radius`
- `--ojos-shadow`

页面和组件不得散落硬编码状态色。新增颜色必须先进入 token 或 `frontend/src/utils/status.ts`。

## 4. 状态颜色

Judge 状态：

- `ACCEPTED`：绿色
- `WRONG_ANSWER`：红色
- `COMPILE_ERROR`：橙色
- `RUNTIME_ERROR`：红色
- `TIME_LIMIT_EXCEEDED`：紫/橙警告系
- `MEMORY_LIMIT_EXCEEDED`：紫/橙警告系
- `OUTPUT_LIMIT_EXCEEDED`：橙红
- `SYSTEM_ERROR`：深红
- `CANCELLED`：灰色
- `UNSUPPORTED_LANGUAGE`：灰色
- `PENDING`：蓝灰
- `JUDGING`：蓝色

模块、worker、health、题目难度和题目可见性状态必须通过 `frontend/src/utils/status.ts` 获取 meta，再由 OJOS tag 组件渲染。

## 5. 组件规范

正式 OJOS 组件位于 `frontend/src/components/oj/`：

- `OjosPageHeader.vue`
- `OjosSection.vue`
- `OjosStatCard.vue`
- `OjosMetricCard.vue`
- `OjosStatusTag.vue`
- `OjosDifficultyTag.vue`
- `OjosLanguageTag.vue`
- `OjosVisibilityTag.vue`
- `OjosHealthBadge.vue`
- `OjosWorkerStatusTag.vue`
- `OjosModuleStatusTag.vue`
- `OjosToolbar.vue`
- `OjosDataTable.vue`
- `OjosCodeBlock.vue`
- `OjosJsonViewer.vue`
- `OjosEmptyState.vue`
- `OjosLoadingState.vue`
- `OjosErrorState.vue`
- `OjosPermissionGuard.vue`

旧 `components/common/StatusTag.vue` 仅作为兼容层，内部委托 `OjosStatusTag`。

## 6. 页面规范

- `/problems`：正式 OJ 题目列表，包含搜索、分页、难度、状态、可见性、标签和限制。
- `/problems/:id`：题面阅读为主，右侧 summary 展示难度、限制、标签和状态。
- `/problems/:id/submit`：语言选择、代码区、题目摘要和提交按钮必须清楚。
- `/submissions`：高密度提交列表，状态、题目、用户、语言、时间、内存和提交时间必须可扫读。
- `/submissions/:id`：状态摘要、case results、debug logs 权限区分清楚。
- `/admin/health`：整体状态大卡、组件状态、延迟和最后更新时间。
- `/admin/judge`：queue summary、workers、tasks、lease 信息和操作按钮。
- `/admin/modules*`：模块 registry、拓扑、详情和 manifest 使用真实 API 展示。

## 7. 禁止事项

- 不写 mock/fake/random 演示数据。
- 不绕过统一 API client。
- 不在响应中展示内部绝对路径、secret、token。
- 不复制其他 OJ 平台的视觉资产或独特样式。
- 不引入重型 UI 框架替换 Naive UI。
- 不把权限不足只做成前端隐藏；后端 401/403 必须正常处理。

## 8. 验收方式

前端修改后至少执行：

```powershell
cd frontend
npm run build
cd ..
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

Docker Control Plane 可用时还必须执行：

```powershell
powershell -NoProfile -File scripts\e2e-api.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123 `
  -WorkerToken $env:OJOS_WORKER_TOKEN
```

人工页面验收结果可写入 `.tmp/agent/reports/ui/`，截图可写入 `.tmp/agent/reports/ui/screenshots/`，不得提交。
