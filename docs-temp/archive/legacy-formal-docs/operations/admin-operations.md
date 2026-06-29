# 管理员操作

> 文档状态：当前实现
> 适用范围：运维 / 管理后台 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS 管理后台中的主要操作、权限要求和审计要求。管理员操作会影响用户权限、题目可管理性、评测队列和 worker 集群，必须有后端权限校验。

## 2. 适用范围

适用于系统管理员、运维人员和维护 admin API 的开发者。主要页面包括 `/admin/health`、`/admin/judge`、`/admin/users`、`/admin/permissions` 和 `/admin/permission-check`。

## 3. 当前实现

当前后台支持查看健康状态、队列、worker、task lease、用户、角色、权限点、授权绑定和审计记录。支持 drain worker、requeue submission、授权/移除 problem owner 和执行 permission check。

## 4. 目标设计

后续可以扩展为更完整的审计、批量授权、模块管理和运维仪表盘，但仍必须保持 fail closed 权限模型。

## 5. 关键流程

管理员发起操作后，前端只负责展示和确认，真实权限判断在后端完成。权限变更写审计日志；requeue 和 drain 也应记录操作者、目标对象和时间。

高风险操作建议使用二次确认：

| 操作 | 影响 | 必要记录 |
| --- | --- | --- |
| 授予用户角色 | 扩大用户权限 | 操作者、目标用户、角色 |
| 移除用户角色 | 收缩用户权限 | 操作者、目标用户、角色 |
| 授予 problem owner | 允许编辑题目和题目包 | problem id、目标用户 |
| drain worker | worker 不再领取新任务 | worker id、原因 |
| requeue submission | 重新评测提交 | submission id、旧状态 |

前端的确认框只减少误操作，不能替代后端审计。后端 API 即使被 curl 直接调用，也必须执行同样的权限检查和审计写入。

## 6. 配置说明

admin API 依赖 Auth 权限系统、Judge API admin 路由和 Gateway 用户上下文。管理员角色和权限点必须来自数据库，不允许前端伪造。

## 7. 安全边界

普通用户不能访问 admin 页面和 API。前端隐藏按钮不是安全边界，直接 HTTP 请求也必须被后端拒绝。

## 8. 验收方式

- 普通用户访问 admin API 返回 403。
- 授权 problem owner 后用户可以编辑对应题。
- 移除授权后直接请求也失败。
- drain worker 后不再领取新任务。

验收时建议准备 admin、用户 A、用户 B。管理员授予 A 某题 owner，A 应能编辑该题，B 仍不能编辑。移除后 A 也不能编辑。对于 worker 操作，drain 后已运行任务继续完成，但 worker 不应 claim 新任务。requeue 后 submission 应生成新的 task attempt 或重新进入可评测状态。

## 9. 常见问题

- 按钮不显示：检查前端 permission guard 和 `/api/auth/me`。
- 后端仍允许操作：检查权限中间件和 resource binding。
- 审计缺失：检查权限写操作是否记录 audit log。

## 10. 相关文档

- [权限管理安全](../security/permission-admin.md)
- [Admin API](../api/admin-api.md)
