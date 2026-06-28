# 模块生命周期

> 文档状态：当前实现，v0 metadata lifecycle
> 最后更新：2026-06-27

## 生命周期动作

Module Installer v0 建模以下动作：

```text
discover
validate
plan
install dry-run
install apply
enable
disable
upgrade dry-run
rollback plan
uninstall dry-run
health
doctor
audit
operation lock
operation history
```

v0 apply 能力聚焦 metadata lifecycle。upgrade apply 默认只允许 demo module metadata-only 场景，rollback apply 默认关闭，uninstall apply 默认关闭或仅限 demo module 且无 dependent。v0 不伪装成完整热升级系统。

## 状态

Registry 中主要状态：

```text
ENABLED
DISABLED
INSTALLING
FAILED_INSTALL
UPGRADING
FAILED_UPGRADE
REMOVED
```

当前 demo module install apply 默认写为 `DISABLED`，随后可 enable / disable。builtin 和 kernel 模块保持 `ENABLED`。

## 计划结构

每个 plan 返回：

```text
actions
affected_tables
affected_modules
dependencies
blocked_by
warnings
dry_run
can_apply
```

如果 `blocked_by` 非空，Gateway 会拒绝 apply 操作；dry-run 仍可返回 plan 供管理员查看。

## 保护规则

- kernel 永远 ENABLED。
- `ojos.judge-core` disable 被拒绝。
- builtin / kernel / judge-core uninstall apply 被拒绝。
- enabled dependent 会阻止 disable / uninstall。
- 涉及业务数据的模块默认不允许真实卸载。

## 操作历史

apply 操作写入：

```text
module_operations
permission_audit_logs
```

失败会保留 operation history，不应只写入容器日志。`operation_id` 全局唯一，lock key 当前为 `module-installer-global`，TTL 默认 300 秒并可配置。过期锁可被新操作接管；成功操作记录 `SUCCEEDED`，失败操作记录 `FAILED`。operation request/result 不保存完整 Authorization、token、secret 或 password。

## v0 / v1 边界

- 已实现：discover、validate、plan、install dry-run、demo module install apply、enable、disable、upgrade plan、rollback plan、uninstall dry-run、health、doctor、operation lock、operation history。
- plan-only：judge-core upgrade、rollback、uninstall。
- 默认关闭：通用 rollback apply、通用 uninstall apply。
- 保护拒绝：kernel disable/uninstall、builtin uninstall、`ojos.judge-core` disable/uninstall。
- v1 目标：签名信任策略、远程发布者信任、完整 upgrade apply、可审计 rollback apply、更细粒度 operation lock。

## Runtime Wiring v1 生命周期语义

Install/apply 写入 registry metadata 和 stored manifest。Enable 让模块进入 active Runtime Snapshot。Disable 保留 registry detail/history，但从 active snapshot 移除该模块的 permissions、menus、frontend routes、gateway routes、health checks 和 topology contribution。

`include_disabled=true` 只供管理员检查，不是 public runtime surface，也不应让 Web Shell 创建 active clickable entry。

Demo module 和 Sample module 生命周期验收验证：

- 已启用 demo module 会出现在 Runtime Snapshot。
- `demo.view` 会在 enable 后进入 active permission registry。
- demo topology metadata 会在 enable 后出现。
- 已禁用 demo module 会从 active Runtime Snapshot 移除。
- 已禁用 demo module 仍可通过 include-disabled 管理检查查看。

## Hotplug L1 生命周期语义

Enable 同时影响 metadata 和 dynamic gateway proxy eligibility。已启用模块可以贡献 active permissions、menus、frontend route metadata、topology、health metadata 和 gateway routes。已禁用模块会从 active Runtime Snapshot 移除，其 gateway routes 不会进入可代理状态。

`POST /api/admin/modules/runtime/reload` 会重建 active route table，并原子替换 Gateway 内存中的 dynamic proxy table。Dry-run 或 include-disabled 视图可以展示 disabled route metadata，但 disabled routes 不能接收流量。

L1 不负责启动或停止模块服务。服务可用性仍来自 compose/operator 管理的部署。L2 只定义受控 runtime driver foundation。

## Hotplug L2 生命周期语义

Module lifecycle 与 service lifecycle 相互独立。安装或启用模块只激活 metadata contribution。L2 foundation 额外暴露 declared services/workers、观测到的 state/health 和 plan-only lifecycle actions。

Service plan actions:

```text
plan-start
plan-stop
plan-restart
plan-reload
plan-health
```

Gateway admin API 只返回计划。Gateway apply 禁用。Metadata-only service 的 start/stop/restart 被阻断，只用于 snapshot/topology 验证。

服务健康失败可以让 dynamic gateway route 进入 `degraded` 或 `unavailable`。`unavailable` 路由不会被代理；Gateway 仍会先执行 auth mode 检查，再返回稳定的 `503`。

## Hotplug L2 Controlled Apply 生命周期

Runtime plan apply 是 operator lifecycle action，与 module install/enable/disable 分离。

Allowed apply path:

```text
Admin/Web -> Gateway -> generate runtime plan
ojosctl/operator -> read plan JSON -> apply trusted compose action
ojosctl/operator -> write operation history/audit
Gateway/Web -> view service state and operation result
```

Gateway/Web apply 明确禁用。管理员可以生成计划、查看 `blocked_by`、`warnings` 和 operation history，但不能通过 Gateway 直接 start、stop 或 restart 服务。

Runtime apply operation states:

```text
PLANNED
APPLYING
SUCCEEDED
FAILED
EXPIRED
BLOCKED
```

Apply 规则：

- 真实 apply 必须提供 `--confirm`。
- `--dry-run` 只打印将要执行的 argv，不执行命令。
- 过期 plan 会被拒绝。
- Metadata-only service 不能 apply。
- service lock 会阻止同一服务并发 apply。
- operation request/result data 入库前必须 redaction。

L2 Controlled Apply 仅限 trusted local compose services，不是通用模块代码执行或部署系统。
