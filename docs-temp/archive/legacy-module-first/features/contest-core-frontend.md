# Contest Core 前端贡献草案

> 文档状态：设计草案，不是已实现前端
> 最后更新：2026-06-27

Contest Core 未来必须使用当前 Web Shell contribution registry。Skeleton 阶段不能依赖 dynamic frontend bundle loading。

## 菜单贡献

| 菜单 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| Contests | `/contests` | `contest.view` | 面向用户的 contest list。 |
| Admin Contests | `/admin/contests` | `contest.manage` | 管理面板贡献。 |

模块禁用后，菜单必须消失或变为 disabled。

## 前端路由

| 路由 | Component Key | 权限 | Skeleton 行为 |
| --- | --- | --- | --- |
| `/contests` | `contest.list` | `contest.view` | Shell fallback 或占位贡献。 |
| `/contests/:id` | `contest.detail` | `contest.view` | component 存在前使用 Shell fallback。 |
| `/contests/:id/problems` | `contest.problem_list` | `contest.view` | 占位路由。 |
| `/contests/:id/submissions` | `contest.submissions` | `contest.view` | 占位路由。 |
| `/contests/:id/scoreboard` | `contest.scoreboard` | `contest.view` | 占位路由，不包含真实 scoreboard。 |
| `/admin/contests` | `contest.admin` | `contest.manage` | 管理页占位。 |

## Shell 必需行为

- Unknown component keys 使用安全 fallback UI。
- Permission guard 由 Web Shell metadata 与 API authorization 共同执行。
- 主菜单、topology 页面和 permission 页面不得增加 sample-specific 或 Contest-specific 硬编码。
- 不加载动态不可信 JavaScript。

## 后续 L3 边界

更完整的 Contest UI 可能需要安全 dynamic frontend bundles。该能力属于 L3 Dynamic Frontend Extension，不属于 Contest Core Skeleton。
