# 权限管理安全

> 文档状态：当前实现
> 适用范围：安全 / 管理后台 / 后端开发
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明权限管理的安全要求，防止只靠前端隐藏按钮或菜单来保护后台能力。

## 2. 适用范围

适用于 `Auth` admin API、前端 admin pages、Problem owner 授权和权限审计。

## 3. 当前实现

权限管理包括用户列表、角色列表、权限点列表、用户角色绑定、problem resource role binding、permission check 和 audit log。

## 4. 目标设计

模块系统上线后，模块应通过 `module.yaml` 声明权限点，安装器注册权限并绑定菜单和路由。

## 5. 关键流程

管理员给用户授予某 problem 的 owner 角色后，用户可编辑该题；移除后，前端按钮和直接后端请求都应失败。

权限变更链路应分为四步：

1. 管理员通过 `/admin/users` 或 `/admin/problems/:id/permissions` 发起授权。
2. Auth API 校验管理员权限，写入用户角色或 resource role binding。
3. Auth API 写入审计日志，记录操作者、目标用户、角色、scope 和时间。
4. Problem API 在编辑、删除、题目包操作时重新执行后端权限检查。

前端权限状态可能因缓存或 token 中旧信息短暂滞后，所以用户体验层面需要刷新 `/api/auth/me`；但最终允许或拒绝必须以后端实时检查为准。

## 6. 配置说明

权限点和角色来自数据库。前端从 `/api/auth/me` 获取当前用户 roles/permissions，但最终判断在后端。

资源级权限需要明确 scope。当前重点是 problem scope，例如给用户授予某个 problem 的 owner 能力。后续模块系统增加 Contest、Group 或 Course 时，应复用同一套 scope 设计，不为每个模块手写一套互不兼容的授权表。

## 7. 安全边界

普通用户不能访问权限管理 API。权限变更必须写审计，不能由前端伪造 roles。

## 8. 验收方式

- 普通用户访问 `/admin/permissions` 返回 403。
- 授权后用户可以编辑对应题。
- 移除授权后用户不能编辑。
- 审计记录可查询。

建议用两个普通用户 A/B 验收：管理员创建 private 题目，只给 A 授权 problem owner。A 应能进入编辑页并提交修改；B 应看不到管理入口，直接请求编辑 API 也失败。移除 A 的授权后，A 刷新页面和直接请求后端都应失败。整个过程应能在 audit log 中看到 grant 和 revoke 记录。

## 9. 常见问题

- 前端显示有权限但后端 403：刷新 `/api/auth/me`。
- 后端允许越权：检查 resource scope 绑定。
- 审计缺失：检查 admin repository 写入。

## 10. 相关文档

- [权限模型](../architecture/permission-model.md)
- [Admin API](../api/admin-api.md)
