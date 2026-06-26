# Contest 模块规划

> 文档状态：目标架构
> 适用范围：架构设计 / 模块开发规划
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 Contest 为什么应作为第一个热插拔验证模块，而不是直接硬编码进 Core Judge Platform。

## 2. 适用范围

适用于模块系统设计、竞赛功能规划、安装器验收和后续产品路线讨论。

## 3. 当前实现

当前仓库没有 Contest 模块。已有能力集中在题目、提交、评测、权限和 worker 管理，这些是 Contest 的基础依赖。

## 4. 目标设计

Contest 应依赖 Kernel identity、Permission、Problem Core、Judge Core、Submission and Result。安装后出现 contest 菜单、API 和 scoreboard；禁用后移除路由和菜单，不破坏 Core。

## 5. 关键流程

Contest 安装流程应由 module installer 读取 manifest，注册权限、菜单、路由、迁移和健康检查，再运行 smoke test。

Contest 作为第一个热插拔模块的原因是它横跨多个基础能力：题目可见性、提交权限、榜单、时间窗口、clarification、管理员操作和前端路由。它足够真实，能暴露模块系统的边界问题；同时它可以依赖 Judge Core，不必修改 worker 协议和评测运行时。

建议路线：

1. 先实现只读 Contest manifest 和拓扑展示。
2. 再实现安装器 v0，只注册路由、权限和菜单。
3. 然后实现 Contest 数据迁移和基础页面。
4. 最后做 enable/disable/reconfigure 验收。

## 6. 配置说明

Contest 未来需要 contest duration、freeze time、scoreboard policy、clarification policy 等配置，不能写死在 Core。

## 7. 安全边界

Contest 权限必须通过权限系统声明，不允许绕过 problem visibility 或 submission 权限。

## 8. 验收方式

以安装、启用、禁用、升级和卸载 Contest 作为模块系统第一个完整 E2E 验证。

验收不要只看菜单出现。应创建 contest，绑定题目，创建普通用户提交，检查榜单权限，禁用模块后确认 contest API 不可访问，重新启用后 contest 数据仍存在。若禁用 Contest 会影响普通题目提交，说明模块依赖边界设计错误。

## 9. 常见问题

- 直接在 Core 写 contest route：会破坏模块边界。
- 禁用后菜单残留：说明前端菜单注册未受模块状态控制。
- scoreboard 越权：检查 contest scope 权限。

## 10. 相关文档

- [模块安装器](module-installer.md)
- [模块生命周期](module-lifecycle.md)
