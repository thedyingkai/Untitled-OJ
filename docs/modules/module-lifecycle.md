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

v0 apply 能力聚焦 metadata lifecycle。upgrade apply、rollback apply、uninstall apply 仅保留 API / 状态机边界，不伪装成完整热升级系统。

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

失败会保留 operation history，不应只写入容器日志。
