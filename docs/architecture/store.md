# Store 与 Deployment 的应用边界

Store 处理可信 Release 的校验、导入、安装和替换。Deployment 处理已有运行实例的启动、停止、重启与卸载。这两类对象不能混为一谈：删除未使用的 Release 元数据不会卸载容器，卸载 Deployment 也不意味着删除可信 Release。

## 代码所有权

| 职责 | 当前入口 | 允许的依赖 |
| --- | --- | --- |
| HTTP 请求、默认值、响应信封和状态映射 | `backend/src/store_v1_api.rs`、`deployment_api.rs` | 应用命令与宿主适配器，不构造安装/替换任务图。 |
| Release 配置、secret 引用、组合输入和计划摘要 | `manager/src/store/config.rs`、`composition.rs` | core 与显式传入的契约，不读取环境、文件、数据库或 HTTP。 |
| 无副作用的 Release 校验 | `manager/src/store/validation.rs` | 只读端口；不提供导入、任务发布或运行时执行能力。 |
| 校验用例的事实读取与运行时预览 | `backend/src/adapters/store_validation.rs` | Catalog、Node、Binding、运行时规划适配器，不引用 HTTP 路由。 |
| Release 导入和元数据删除 | `backend/src/store/metadata.rs` | 可信注册表、Console 兼容入口与历史证明。 |
| 安装与升级/回滚编排 | `backend/src/store/install.rs`、`replacement.rs` | 明确的规划能力与统一提交入口，保留现有持久化顺序。 |
| 提交协调与失败撤销 | `backend/src/store/admission.rs` | 持有 Store 协调锁；调用持久存储和 OperationCoordinator。 |
| 运行实例生命周期 | `backend/src/deployment.rs` | 活跃 Binding 约束、Contribution 卸载补偿与 OperationCoordinator。 |

`backend/src/store` 是宿主侧应用编排，并非整个目录都是纯领域层。`StoreError` 暂时保留既有状态码以维持 API 契约；纯规则使用 manager 的 `StoreRuleError`。安装/替换仍需要 Console、持久存储与运行时契约。将这些依赖伪装成通用 Repository 不会使边界更清楚。

## 如何定位规划代码

- `bindings` 读取并选择 API provider；`topology` 形成修订、Binding generation 与上下文切换计划。
- `runtime_plan` 构造类型化 runtime pipeline；`service_context` 处理 workload 身份、事件与保留卷。
- `node` 提供 Node facts 和运行时能力约束；`placement` 处理 endpoint、Deployment 和活跃 mutation 冲突。
- `artifacts` 处理离线 OCI 内容；`history` 证明历史 Release 和回滚目标；`contribution` 接入插件贡献的任务依赖。
- `commands` 保留既有命令字段与兼容别名；`context` 只携带 mutation 所需的幂等标识和现有时间/标识算法。

这些模块显式导入各自依赖，不通过共享的 `super::*` 隐藏依赖。HTTP 不再向规划模块提供辅助函数。

## 安装与替换的提交顺序

```text
可信 Catalog 与 Node/Binding 事实
        → 配置、组合、运行时与补偿计划
        → StoreAdmission 持有协调锁
        → 重新检查 Deployment/endpoint/活跃 mutation 冲突
        → 按计划顺序占用 Topology revision
        → OperationCoordinator plan / confirm / enqueue
        → 构造接受响应，再释放协调锁
```

拓扑占用失败时逆序撤销本次已成功取得的占用；任务发布失败时同样撤销本次占用。占用其他 Operation 的 Topology 不被清除。底层 Operation/Job 的恢复仍由原有协调器负责。

这里不是覆盖所有存储对象的大事务。Release 元数据发布、Topology revision 的建立和 Operation 记录原本就有各自的持久化步骤，迁移没有改变这些步骤的相对顺序。`StoreAdmission` 也只是进程内 Store 提交锁，不替代远程控制面的单写者约束、数据库 CAS 或租约 fencing。

HTTP 幂等重放与底层 Topology 占用是不同边界：已占用的 Topology 再次 `begin` 仍返回冲突；OperationCoordinator 对同一计划的重放保留原任务身份。不能在重构时为追求表面“幂等”跳过现有占用检查。

## 验收边界

软件验证放在独立仓库外套件中，针对当前源码的临时副本运行。产品构建与打包 workflow 不运行这些验证。SQLite 回归、纯规则回归和编译不能替代 PostgreSQL、真实 Agent、Docker 与外部 provider 的集成验收；实际执行范围记录在仓库外报告中。
