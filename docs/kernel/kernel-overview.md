# Kernel Overview

> 文档状态：当前实现，Phase 1 skeleton
> 适用范围：Kernel 开发 / 模块开发 / 架构评审
> 最后更新：2026-06-27

OJOS Kernel 负责模块系统的系统级能力。它不包含题目、比赛、训练、讨论或远程 OJ 的业务逻辑。

## Kernel 能力

```text
Module Installer
Module Registry
Module Runtime
Module Lifecycle
Module Topology
Module Health
Module Policy
Module Audit
Module Config
Module Package Verification
Module Dependency Resolver
Module Operation Lock
Module Operation History
```

## Kernel Built-ins

```text
ojos.kernel.installer
ojos.kernel.module-runtime
ojos.kernel.module-registry
ojos.kernel.topology
ojos.kernel.policy
ojos.kernel.audit
ojos.kernel.config
ojos.kernel.health
```

Kernel built-ins 不允许 disable 或 uninstall。

## Platform Built-ins

```text
ojos.platform.gateway
ojos.platform.web-shell
ojos.platform.identity-access
ojos.platform.storage
ojos.platform.observability
```

Platform built-ins 默认受保护，不作为普通 feature module 卸载。

## Feature Modules

```text
ojos.judge-core
ojos.demo-module
future ojos.contest
future ojos.training
future ojos.group
future ojos.remote-oj
```

Feature modules 根据 dependency、dependent 和 safety policy 判断 enable、disable、upgrade、rollback 和 uninstall。
