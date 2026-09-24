# 执行契约与 Docker 驱动

控制面规划任务并持久化期望状态，Node Agent 领取任务并执行 Docker 操作。两端共享数据契约，不共享运行时实例。

| 所有者 | 职责 | 不负责 |
| --- | --- | --- |
| `protocol/src/execution` | 容器描述、Service Context、Release 安装/替换、资源请求、迁移身份与观测；确定性校验和摘要 | 连接 Docker、读取凭据、访问文件/数据库/网络、执行容器 |
| `runtime` | `ContainerRuntime`、Docker Engine API 映射、实际容器与卷操作、inspect 结果核对、节点本地凭据 | 控制面的任务持久化、业务规划、选择目标节点 |
| `agent` | Job 领取与执行、ledger、资源物化、凭据更新、调用 runtime | 替控制面修改业务 Spec 或自行扩大任务权限 |
| `backend` / `storage` | 规划、发布、保存与投影共享契约 | 为了引用载荷类型而依赖 Docker 驱动 |

## 模块归属

- `container`：OCI 引用、容器描述、端口和纯本地配置校验。
- `volumes`：卷身份、保留策略和用于重放核对的确定性标签。
- `service_context`：无凭据的 Binding、事件、公开验签材料和上下文 CAS 输入。
- `migration`：一次性迁移身份、资源名称摘要和封闭观测结果。
- `pipeline`：安装、替换、资源回收及 provider 步骤的序列化描述。
- `health`：根据显式观测和策略判断健康门槛，不自行探测或读取时钟。
- `validation`：协议与驱动共用的封闭标识符校验。

`ContainerSpec` 中的 `runtime_context` 和 `resource_secret_file_mounts` 仍标记为 `serde(skip)`，只允许 Agent 在接收任务之后根据本地策略补充。迁移类型所有权不让控制面获得指定宿主路径、凭据或任意 Docker 参数的能力。`WorkloadCredential` 仍位于 runtime，不具备序列化能力。

本次迁移保留既有字段、serde 属性、默认值、错误和规则。`orchestrator_runtime` 继续重导出共享契约，已有源码调用路径可兼容；正式控制面和存储代码直接导入 protocol。去掉这两层的 runtime 依赖不等于删除 Docker 支持：Agent 的 runtime 依赖和 Docker Engine 实现均保留。

共享协议只使用序列化、字符串/URL 解析、摘要和错误类型等本地计算依赖。纯 `Path` 校验不访问文件系统；任何文件存在性、权限检查和实际物化都属于 Agent/runtime。
