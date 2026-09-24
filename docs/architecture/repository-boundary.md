# 仓储边界与历史兼容

`orchestrator-storage` 拥有 `OrchestratorStore` 仓储接口及 Memory、Shared、SQLite、PostgreSQL 实现。正式存储代码不依赖旧 Console，也不依赖 Docker 驱动；它使用 core 的领域类型和 protocol 的执行契约。

```text
backend ────────────────► storage ─────► core / protocol
legacy Console ─────────► storage
Agent ──────────────────► runtime ─────► Docker Engine
```

这张图只描述仓储和执行边界。backend/manager 仍有尚未迁出的 Console 应用调用，不能由 storage 已解耦推断整个正式应用层已经隔离 legacy。

## 所有权

- `storage/src/repository/mod.rs`：共同仓储接口、默认组合操作和兼容方法。
- `repository/memory.rs`：显式临时开发状态，不充当持久库的写后全表镜像。
- `repository/shared.rs`：共享一个真实仓储实例。克隆共享句柄，不复制数据库记录；锁限于单次仓储方法调用。
- `storage/src/sqlite.rs`、`postgres_store.rs`：连接、事务、持久记录与约束。跨记录原子写仍由各数据库实现负责，不能把共享句柄的互斥锁当成数据库事务。
- `core/src/node_registry.rs`：节点树校验、祖先/后代和 API 可见路由的确定性计算；只处理显式传入的记录。
- `core/src/log_source.rs`：日志声明的服务归属与路径规则，不读取日志文件。

`OrchestratorStore` 暂时保留现有宽接口；按用例收窄读取能力应在实际应用迁移时完成，不在本次保持行为的搬迁中重写每个调用方。

## 保持的行为

接口方法、默认组合写的失败恢复、Shared 的单方法串行化，以及 SQLite/PostgreSQL 的事务实现均保留。`legacy` 继续重导出三种仓储类型，已有源码引用不需要立即变更。

内存节点 upsert 与持久化节点 upsert 原来具有略有不同的校验顺序。迁移保留两个明确入口，避免在归属调整时改变错误优先级；SQLite 和 PostgreSQL 原本相同的图校验现在调用同一 core 函数。

旧数据导入仍在 storage 的 `legacy_import` 模块，保留一次性导入与重复打开的幂等语义。这里的“历史兼容”不代表允许正式 v1 操作回退到旧的 Docker Compose、进程执行或 Deferred provider。

## 后续应用迁移

正式 Store 的 Release 元数据删除仍通过 Console 动作调度；部分诊断请求仍经过旧路由投影。它们需要按实际用例迁移，保留响应、Operation 记录和错误契约。仅把 Console 改名，或在新对象中继续调用原来的 `dispatch`，不算完成隔离。
