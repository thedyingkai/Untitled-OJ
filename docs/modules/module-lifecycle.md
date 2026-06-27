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
