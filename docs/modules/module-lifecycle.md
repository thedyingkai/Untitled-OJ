# 模块生命周期

> 文档状态：部分实现
> 适用范围：架构设计 / 模块开发规划 / 运维
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义模块从发现、安装、启用、禁用、升级到移除的状态机。状态机用于让安装器和管理后台可解释、可恢复、可审计。

## 2. 适用范围

适用于 module installer、module registry、模块管理页面和运维手册。

## 3. 当前实现

当前仓库已实现 `module_installations` 表和 builtin 模块状态展示。Kernel 内置模块与 `ojos.judge-core` 通过 bootstrap 以 `ENABLED` 状态登记。尚未实现 installer，也不支持安装、禁用、启用、升级、卸载操作。

## 4. 目标设计

当前只读 registry 实际使用 `ENABLED` 展示 builtin 模块。目标状态包括：`DISCOVERED`、`INSTALLING`、`ENABLED`、`DISABLING`、`DISABLED`、`ENABLING`、`UPGRADING`、`FAILED_INSTALL`、`FAILED_UPGRADE`、`UNINSTALLING`、`REMOVED`。

## 5. 关键流程

安装失败进入 `FAILED_INSTALL`，升级失败进入 `FAILED_UPGRADE`。拥有数据的模块默认 disable 而不是硬删除。真正删除数据必须显式确认并审计。

状态迁移应是单向受控的，不允许任意跳转。典型流程：

```text
DISCOVERED -> INSTALLING -> ENABLED
ENABLED -> DISABLING -> DISABLED
DISABLED -> ENABLING -> ENABLED
ENABLED -> UPGRADING -> ENABLED
INSTALLING -> FAILED_INSTALL
UPGRADING -> FAILED_UPGRADE
DISABLED -> UNINSTALLING -> REMOVED
```

失败状态不是终点。管理员可以查看失败原因，修复配置或依赖后重试。重试前必须确认上一次失败留下的迁移、权限、路由和菜单是否已经回滚或处于可恢复状态。

## 6. 配置说明

生命周期记录应包含 module id、version、状态、失败原因、操作者、时间和关联迁移版本。

## 7. 安全边界

只有管理员可以改变模块状态。禁用模块不能破坏 Kernel 和 Core 的基础能力。

## 8. 验收方式

当前验收是确认 `/admin/modules/:id` 能展示安装状态、manifest 和组件。模拟安装失败、升级失败、禁用、启用和卸载是 installer v0 之后的验收内容。

验收时应重点关注“中断恢复”。例如安装过程中数据库迁移失败，系统应能展示失败模块、失败步骤和错误信息；再次安装不应重复写入已注册权限；禁用模块后，已有业务数据应保留，但 public route 和菜单入口应关闭。

## 9. 常见问题

- 禁用后 API 仍可访问：检查 Gateway route 注册状态。
- 升级失败后状态不清晰：检查事务和失败记录。
- 删除数据误操作：应改为默认 disable。

## 10. 相关文档

- [模块安装器](module-installer.md)
- [模块清单](module-manifest.md)
