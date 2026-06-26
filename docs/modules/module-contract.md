# 模块契约

> 文档状态：部分实现
> 适用范围：架构设计 / 模块开发规划
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义 OJOS module 的组成和边界。当前已经实现 Module Registry v0 的只读数据模型和拓扑查询；installer、启用/禁用/升级/卸载仍属于后续目标。

## 2. 适用范围

适用于设计新模块、编写 `module.yaml`、实现安装器和维护模块依赖关系的开发者。

## 3. 当前实现

当前仓库已经新增 `module_sets`、`module_nodes`、`module_edges`、`module_components`、`module_installations` 等表，并由 Gateway 启动时幂等 bootstrap 内置模块。`modules/judge-core/module.yaml` 已声明当前 Judge Core 的真实服务、权限、路由、storage、健康检查和迁移。现阶段只读展示模块拓扑，路由、权限、菜单和迁移仍由现有系统管理，不由 installer 动态安装。

## 4. 目标设计

一个模块应声明后端服务、前端路由、菜单、权限点、数据库迁移、配置 schema、健康检查、storage 声明、worker 服务和验收脚本。模块之间必须显式声明依赖。

## 5. 关键流程

当前关键流程是 bootstrap 和只读查询：Gateway 启动后把 Kernel 内置模块和 `ojos.judge-core` 写入 module registry；管理员通过 `/api/admin/modules`、`/api/admin/modules/topology` 查看集合、节点、依赖和组件。未来安装流程才会解析 manifest、检查依赖、注册能力并执行 smoke test。

模块契约建议分为以下层次：

| 层次 | 必须声明的内容 | 验收重点 |
| --- | --- | --- |
| identity | `id`、`name`、`version`、`set` | id 稳定、版本可比较 |
| dependency | 依赖模块、平台版本 | 缺依赖时拒绝安装 |
| backend | API、服务、迁移、健康检查 | 路由不冲突、迁移可回滚 |
| frontend | 路由、菜单、页面入口 | 禁用后不可见且不可访问 |
| permission | 权限点、角色、scope | 后端权限校验存在 |
| operations | smoke test、backup note、指标 | 运维可观测 |

契约的核心原则是“声明即边界”。如果一个能力没有在 manifest 中声明，安装器不应隐式公开它；如果一个模块声明了权限点，后端必须能执行对应检查。

## 6. 配置说明

模块配置应由 manifest 声明 schema，实际值由环境变量或部署配置提供。模块不得携带生产 secret。

## 7. 安全边界

模块不能绕过 Gateway、内部 HMAC 和权限系统。模块暴露的 public API 也不能返回内部路径。

## 8. 验收方式

当前验收以 Module Registry v0 为准：bootstrap 幂等、普通用户访问 admin modules API 被拒绝、topology 返回 nodes/edges/components、前端 `/admin/modules` 和 `/admin/modules/topology` 使用真实 API。installer 验收留到下一阶段。

具体测试用例包括：重复安装同一模块应幂等；安装缺少依赖的模块应失败；两个模块声明同一路由应失败；迁移执行失败应进入失败状态并保留日志；禁用模块后菜单、路由和后端 API 都不可用；重新启用后数据不丢失。

## 9. 常见问题

- 模块依赖缺失：安装器应拒绝安装。
- 路由冲突：安装前检测并失败。
- 数据迁移失败：进入失败状态并保留日志。

## 10. 相关文档

- [模块清单](module-manifest.md)
- [模块安装器](module-installer.md)
