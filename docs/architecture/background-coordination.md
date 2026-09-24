# 后台协调的职责与恢复边界

`backend/src/topology_worker.rs` 只保留后台主循环和独立的租约恢复循环。对外使用的 `process_one`、运行时 Binding 投影与路由保留判断仍从原模块路径导出；服务启动、Agent 回调和关停调用不需要知道内部拆分。

## 模块所有权

| 模块 | 负责 | 不负责 |
| --- | --- | --- |
| `core/src/binding_projection.rs` | Binding generation、激活/撤销、跨 Topology 所有权移动和投影增减判断。时间和当前记录均由调用方提供。 | 数据库、租约、网络、环境和时钟读取。 |
| `topology_worker/lease.rs` | control-plane Job heartbeat、进度检查、完成与 Operation 投影。 | 更改任务业务含义，替代存储 fencing。 |
| `topology_worker/recovery.rs` | 过期租约、终态 Topology 占用和可恢复 Operation 的修复。 | 正常请求中的重复扫描、无证据地认定副作用成功。 |
| `topology_worker/apply.rs` | 按既有 Job kind/phase 执行拓扑应用与组提交，并协调补偿。 | HTTP 解析、网络探测池的调度。 |
| `topology_worker/projection.rs` | 根据持久 Binding 和 runtime 证据协调 Gateway/Auth 实际授权投影。 | 修改不可变 TopologySpec，因短暂健康失败撤销恢复所需路由。 |
| `topology_worker/reconciliation.rs` | 周期性汇总 runtime、provider 和网络观测，持久化 TopologyStatus。 | 自动安装服务、改变期望拓扑。 |
| `topology_worker/network.rs` | 有界网络探测池、响应限制、观测年龄与探测结果。 | 授权投影、任务租约恢复。 |
| `topology_worker/external_health.rs` | 无 Agent 的 External endpoint 初次验证与后续健康刷新。 | 伪造 managed runtime attestation。 |
| `topology_worker/node_lifecycle.rs` | Node drain/remove 的持久约束与执行。 | Store 安装或拓扑隐式服务删除。 |
| `topology_worker/observation.rs` | runtime 协议状态到拓扑展示状态的纯转换。 | 网络和存储读取。 |

`payload` 定义控制面 Job 的既有序列化输入；`binding_health` 负责应用前的严格健康门槛；`context` 保留控制面节点标识、时间标记和错误详情界限。模块之间显式导入，探测与协调通过纯 observation 转换共享展示规则，不相互依赖。

## 保持不变的恢复规则

- `run_lease_recovery_loop` 仍是周期性过期租约恢复的唯一所有者。Agent claim 不触发全表恢复扫描。
- heartbeat 仍每 10 秒执行，连续 25 秒没有处理进展时停止续租；业务步骤继续使用原有 checkpoint。
- Topology revision 和 Binding 的最终写入仍通过存储层的 lease fencing 与 CAS。拆分模块不会延长失效 worker 的写入权限。
- 对未知副作用保持 `NEEDS_ATTENTION`，不把未知结果变成可安全重试或成功。
- 投影撤销仍先撤 Gateway、后撤 Auth；授予仍先 Auth、后 Gateway。混合变更先收缩到安全交集，再授予目标集合。
- 初次 Binding 应用使用严格健康门槛；已经激活的 Binding 在短暂健康/上报异常时保留恢复路由，但期望停止/删除、结构漂移或 attestation 失败仍撤销授权。

## 持久化边界

`DurableStore` 继续统一 SQLite/PostgreSQL 的宿主调用，不新增一层通用 Repository。原先藏在适配器中的 `stage_binding_generations` 已委托给 core；数据库查询、时间取样、事务与错误映射仍在原适配器。`TopologyApplyGroupMember` 的数据定义由 core 所有，storage 保留原公开路径的重导出；SQL 与序列化契约不变。

模块拆分的验收包含函数体比对、无循环依赖检查以及仓库外的 worker、Binding generation 和核心回归。真实 PostgreSQL、远端 Agent 与 Gateway/Auth 部署验收需要相应服务，不能用本地编译或 SQLite 回归代替。
