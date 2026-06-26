# 模块清单 module.yaml

> 文档状态：部分实现
> 适用范围：架构设计 / 模块开发规划
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义 `module.yaml` 基本结构。当前 `modules/judge-core/module.yaml` 已作为 Judge Core builtin module 的正式 manifest，用于 Module Registry v0 的拓扑登记；安装、启用、禁用、升级仍未实现。

## 2. 适用范围

适用于编写 Judge Core、Contest、Training 等模块 manifest 的开发者，以及实现 module installer 的开发者。

## 3. 当前实现

当前仓库已新增 `modules/judge-core/module.yaml`，并由 Gateway bootstrap 将对应信息写入 module registry 表。该流程是 builtin module 登记，不是 installer 安装；B Contest、Training、Remote OJ 等模块尚未开始。

## 4. 目标设计

manifest 必须声明模块 id、版本、集合、依赖、提供的权限、服务、前端路由、Gateway route、迁移和健康检查。

## 5. 示例

```yaml
id: ojos.judge-core
name: Judge Core
version: 0.1.0
set: core-capability
kind: feature
status: builtin
requires:
  modules:
    - ojos.kernel.edge-ui-shell >= 0.1.0
provides:
  permissions:
    - problem.view
    - problem.edit
    - judge.submit
    - submission.view.own
  backend_services:
    - id: problem-api
      path: services/problem-api
      exposure: internal
  frontend:
    routes:
      - /problems
      - /submissions
  gateway_routes: []
  health_checks: []
```

## 6. 字段说明

- `id` 必须稳定且全局唯一。
- `version` 必须可比较。
- `requires` 必须列出模块依赖。
- `provides.permissions` 必须列出权限点。
- `gateway_routes` 必须绑定服务和权限。
- `migrations` 必须可追踪，不能手工改库后跳过。

建议将 manifest 校验拆成 schema 校验和语义校验。schema 校验负责字段类型、必填项、枚举值；语义校验负责依赖是否存在、版本是否满足、route 是否冲突、权限点是否重复、迁移版本是否连续。两类校验都应在执行任何副作用前完成。

manifest 中可以声明默认配置项，但不应声明生产 secret 的默认值。比如可以声明 `OJOS_WORKER_TOKEN` 是必填项，不能给出可用于生产的固定 token。

当前 Judge Core 的权限键必须以现有权限迁移和业务逻辑为准，例如 `problem.edit`、`problem.manage.data`、`submission.view.own` 和 `submission.view.all`。不要使用旧草稿中的 `problem.update`、`problem.package.manage`、`judge.submission.*` 或 `judge.admin`。当前重测和取消提交能力由 `problem.manage.data` 的题目级权限保护；评测集群后台页面的访问条件是 Kernel 权限 `system.admin`。

## 7. 安全边界

manifest 不能包含生产 secret。模块声明 route 不代表自动公开，安装器必须检查权限和冲突。

## 8. 验收方式

当前验收是确认 `modules/judge-core/module.yaml` 与真实路径一致，并确认 Module Registry v0 能展示该 manifest。安装器实现后，才执行 schema 校验、依赖校验、冲突检测和 smoke test。

验收时至少准备三个 manifest：一个合法模块、一个缺依赖模块、一个与合法模块路由冲突的模块。合法模块应安装成功；缺依赖模块应在副作用前失败；冲突模块应返回可读错误并保持系统状态不变。

## 9. 常见问题

- id 改名导致升级失败：模块 id 必须稳定。
- 路由冲突：安装前应拒绝。
- 权限未声明：前端菜单和后端 API 不能上线。

## 10. 相关文档

- [模块契约](module-contract.md)
- [模块安装器](module-installer.md)
