# 仓储边界与历史兼容

`orchestrator-storage` 拥有 `OrchestratorStore` 仓储接口及 Memory、Shared、SQLite、PostgreSQL 实现。正式存储代码不依赖旧 Console，也不依赖 Docker 驱动；它使用 core 的领域类型和 protocol 的执行契约。

```text
backend ────────────────► storage ─────► core / protocol
legacy Console ─────────► storage
Agent ──────────────────► runtime ─────► Docker Engine
```

这张图描述仓储和执行边界。正式 v1 Store/Catalog/诊断使用 `backend/src/registry`，不持有 Console；历史文件加载和投影转换仍通过显式适配器复用 legacy，不能把应用调度解耦等同于整个 crate 依赖已消除。

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

## 正式应用与兼容入口

`backend/src/registry/RegistryContext` 只拥有 schemas、共享仓储句柄和启动告警。持久请求克隆句柄，不构造旧执行控制台、不加载第二份内存镜像。显式临时开发模式仍串行处理复合请求。

`registry/operations.rs` 只接受 Release 元数据删除和当前拓扑诊断两个类型化入口。它保留既有计划、确认、Operation 锁、逐步日志、失败记录和删除前的恢复快照；没有通用动作入口，也不构造 runtime/provider。安装、升级、回滚和卸载继续由 Store/Deployment 用例持久发布 Job，交给 Agent 执行。

`diagnostics_api.rs` 处理诊断传输；`api_v1.rs` 保留鉴权、幂等、审计和响应封装，不再回退到 0.2 动作路由。

兼容职责分为两个明确入口：

- `adapters/registry_compat.rs` 复用历史文件解析、Release 投影、已部署视图和诊断格式，不调用 Console 或动作调度。后续可逐步迁移这些格式的所有权，不混入本次保持行为的改造。
- `adapters/legacy_console.rs` 只供显式 0.2 路由及已有嵌入宿主 API 使用。转换共享同一仓储；旧入口写入后，正式 v1 和其他请求立即读取同一状态。公开的 `start_embedded_server_with_console` 保留兼容，启动时只转换一次。

旧 Console 的动作实现仍为历史调用方保留，不属于正式 v1 的运行路径。新的仓储操作与旧实现分别在仓库外对比成功、失败、锁冲突、恢复快照及诊断输出，避免仅按文件归属推断行为等价。
