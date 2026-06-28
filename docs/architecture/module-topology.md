# 模块拓扑

> 文档状态：当前实现与发布边界
> 适用范围：架构、模块开发、运行时验收
> 最后更新：2026-06-28

本文说明 OJOS v0.1.0 的模块拓扑模型。历史长版设计已归档到 `docs/archive/legacy-docs/root-module-topology-original.md`，正式文档只描述当前可验收能力和明确边界。

## 拓扑层级

OJOS 使用四层拓扑：

```text
Service / Worker
  -> Module
  -> Module Set
  -> OJOS Runtime
```

- `Service / Worker`：可观测、可健康检查、可被 runtime plan 描述的进程或任务单元。
- `Module`：通过 `module.yaml` 声明权限、菜单、前端路由、Gateway 路由、服务、worker、健康检查、事件、存储桶和拓扑节点。
- `Module Set`：模块集合，用于表达 Kernel、Platform、Feature、Sample 等归属。
- `OJOS Runtime`：Installer、Registry、Runtime Snapshot、Gateway 与 Web Shell 共同读取的运行态视图。

## 当前已实现

v0.1.0 发布基线中，拓扑由 manifest 与 registry 生成，并通过管理 API 与 Web Shell 展示：

- `module_sets`、`modules`、`module_dependencies`、`module_components`、`module_installations` 等 registry 表记录模块元数据。
- `modules/judge-core/module.yaml` 把题库、提交、评测与 worker 能力包装为 `ojos.judge-core` feature module。
- `modules/sample-hello/module.yaml` 作为 SDK 样例模块验证普通模块接入。
- Gateway 暴露 `/api/admin/modules`、`/api/admin/modules/topology`、`/api/admin/runtime/snapshot`、`/api/admin/runtime/routes`、`/api/admin/runtime/services`。
- Web Shell 从 Runtime Snapshot 展示模块中心、贡献、权限、拓扑、routes、services 与 operations。
- `ojosctl` 和 `ojos-installer-tui` 是官方原生安装与运维入口。

## Runtime Snapshot 关系

Runtime Snapshot v1 是拓扑的主要事实来源。它包含：

- 已启用模块和 registry 中可见模块。
- manifest 贡献的 permissions、roles、components、menus、frontend routes、gateway routes、services、workers、health checks、storage buckets、events、scheduled jobs、admin panels。
- topology nodes 与 edges。
- route table 和 runtime services 的输入数据。

Gateway 与 Web Shell 不直接发明模块拓扑。它们只读取 snapshot 或 registry 视图，并按权限显示。

## Gateway 路由拓扑

Gateway route 只允许绑定到受信任 `service_id`，不接受模块 manifest 提供任意 `target_url`。路由拓扑包含：

- `prefix`
- `service_id`
- `auth_mode`
- `required_permission`
- `enabled`
- 冲突和 reserved prefix 检查结果

禁用路由可以出现在 registry 和 runtime 管理视图中，但不能被代理。

## Web Shell 贡献拓扑

Web Shell 从 snapshot 读取菜单、前端路由元数据、admin panel、permission registry 和 topology 数据。v0.1.0 不动态加载不可信前端 bundle；未知 component 使用安全 fallback。Web Shell 不执行安装、启用、禁用或 runtime apply。

## 服务拓扑

服务和 worker 可以通过 manifest 声明为 metadata、trusted compose service 或后续 runtime driver 目标。v0.1.0 支持服务状态、健康检查、plan-only lifecycle 和 controlled apply 基础：

- metadata service 只展示，不允许执行 host lifecycle。
- trusted service 必须来自受信任服务表或 compose allowlist。
- runtime plan 可由 `ojosctl` 生成和 dry-run。
- 危险 apply 必须显式确认，并遵守 controlled apply 规则。

## 当前边界

以下能力不属于 v0.1.0 当前完成范围：

- L3 dynamic frontend bundle。
- L4 full module hotplug。
- 远程模块市场。
- hook 执行。
- package signature / trust policy 完整实现。
- true multi-machine runtime apply。
- Judge Core GA。
- Contest、Training、Remote OJ 等真实业务模块实现。

## 验收入口

拓扑相关能力通过以下命令验收：

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\e2e-module-compat.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123
```

验收必须保持 `path_leaks=0`，普通用户访问管理端点返回 `403`，无 token 返回 `401`。
