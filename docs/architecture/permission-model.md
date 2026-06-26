# 权限模型

> 文档状态：当前实现
> 适用范围：架构设计 / 安全 / 管理后台
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS 当前权限模型，包括角色、权限点、资源级绑定和后端强制校验。

## 2. 适用范围

适用于 Auth admin API、Problem owner 授权、前端权限显示和安全审计。

## 3. 当前实现

当前支持 system scope 和 problem scope。管理员可以查看用户、角色、权限点，并授予或移除 problem owner。

## 4. 目标设计

模块系统上线后，模块通过 `module.yaml` 声明权限点，安装器注册权限、菜单和路由绑定。

## 5. 关键流程

授予 problem owner 后，用户获得该 problem 的编辑和题目包管理能力；移除后直接后端请求也必须失败。

## 6. 配置说明

权限点、角色和绑定存在数据库中。前端只消费 `/api/auth/me` 返回的 roles/permissions。

## 7. 安全边界

前端 permission guard 不是安全边界。所有后端 mutation API 必须检查系统权限或资源权限。

## 8. 验收方式

授权、编辑、移除、再次编辑失败，形成完整闭环；普通用户不能访问权限管理页面和 API。

## 9. 常见问题

- 授权后不生效：检查 resource scope 是否正确。
- 前端显示旧权限：刷新当前用户信息。
- API 越权：检查后端权限中间件。

## 10. 相关文档

- [权限管理安全](../security/permission-admin.md)
- [Admin API](../api/admin-api.md)
