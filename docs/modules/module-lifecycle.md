# Module Lifecycle

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

## Plan

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

## Runtime Wiring v1 Lifecycle Semantics

Install/apply writes registry metadata and stored manifest data. Enable changes the module into active Runtime Snapshot scope. Disable keeps registry detail/history, but removes the module's permissions, menus, frontend routes, gateway routes, health checks and topology contributions from the active snapshot.

`include_disabled=true` is the admin inspection escape hatch. It is not a public runtime surface and should not be used by Web Shell to create active clickable entries.

Demo module lifecycle acceptance now verifies:

- enabled demo module appears in Runtime Snapshot;
- `demo.view` appears in active permission registry after enable;
- demo topology metadata appears after enable;
- disabled demo module is excluded from active Runtime Snapshot;
- disabled demo module remains visible through include-disabled admin inspection.

## Hotplug L1 Lifecycle Semantics

Enable now affects both metadata and dynamic gateway proxy eligibility. An enabled module can contribute active permissions, menus, frontend route metadata, topology, health metadata and gateway routes. A disabled module is excluded from the active Runtime Snapshot and its gateway routes are not proxy-enabled.

`POST /api/admin/modules/runtime/reload` rebuilds the active route table and atomically replaces Gateway's in-memory dynamic proxy table. Dry-run or include-disabled views can show disabled route metadata, but disabled routes must not receive traffic.

L1 does not start or stop module services. Service availability still comes from compose/operator-managed deployment. L2 will define the controlled runtime driver.

## Hotplug L2 Lifecycle Semantics

Module lifecycle and service lifecycle are separate. Installing/enabling a module activates metadata contributions. L2 foundation additionally exposes declared services/workers, their observed state/health and plan-only lifecycle actions.

Service plan actions:

```text
plan-start
plan-stop
plan-restart
plan-reload
plan-health
```

The Gateway admin API returns plans only. Apply is disabled in Gateway. Metadata-only services are blocked from start/stop/restart and remain useful only for snapshot/topology validation.

A service health failure can make a dynamic gateway route `degraded` or `unavailable`. Unavailable routes are not proxied; Gateway still enforces auth mode before returning a stable 503.

## Hotplug L2 Controlled Apply Lifecycle

Runtime plan apply is now an operator lifecycle action, separate from module install/enable/disable.

Allowed apply path:

```text
Admin/Web -> Gateway -> generate runtime plan
ojosctl/operator -> read plan JSON -> apply trusted compose action
ojosctl/operator -> write operation history/audit
Gateway/Web -> view service state and operation result
```

Gateway/Web apply is intentionally disabled. Admin callers can generate plans, inspect `blocked_by` and `warnings`, and view operation history. They cannot directly start, stop, or restart services through Gateway.

Runtime apply operation states:

```text
PLANNED
APPLYING
SUCCEEDED
FAILED
EXPIRED
BLOCKED
```

Apply rules:

- `--confirm` is required for real apply.
- `--dry-run` prints the argv that would run and does not execute it.
- Expired plans are rejected.
- Metadata-only services cannot be applied.
- A service lock prevents concurrent apply for the same service.
- Operation request/result data is redacted before being stored.

L2 Controlled Apply remains limited to trusted local compose services. It is not a generic module code execution or deployment system.
