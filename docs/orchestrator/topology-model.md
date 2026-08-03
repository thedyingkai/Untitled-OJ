# Topology v1 模型与执行语义

Topology v1 描述已经注册或部署的服务之间的期望连接关系。它不负责安装服务，也不把 Operation、日志、诊断或画布坐标混入业务拓扑。

## 三类持久对象

### `TopologySpec`

Spec 只保存期望状态：

- `topology_id`、root Endpoint 和 authority；
- Endpoint 的服务引用、协议、健康检查路径及声明式配置；
- Link 的源/目标 Endpoint、协议、认证模式、scope 和声明式策略。

Spec 中引用的 `service_id` 必须已经注册。Endpoint ID、root、Link 引用和重复项在创建 revision 前统一校验。运行健康、容器 ID、Operation ID、日志、诊断及 UI 布局均不得写入 Spec。

### `TopologyRevision`

Revision 是不可变对象，包含单调递增的 revision number、父 revision、内容 SHA-256、创建者和变更说明。当前 draft 通过强 ETag 暴露；创建下一 revision、apply 和 rollback 必须提交对应的 `If-Match`。陈旧 ETag 返回 `409 TOPOLOGY_REVISION_CONFLICT`。

Diff 对指定的两个 revision 执行规范化、确定性比较。同一输入必须得到字节级稳定的变更序列。

Rollback 不移动历史指针，也不修改旧 revision。它复制目标 Spec，创建一个带 `rollback_of_revision_id` 的新 revision，然后走一次正常 apply。

### `TopologyStatus`

Status 保存真实观测状态：

- desired/observed revision；
- `RECONCILING`、`IN_SYNC`、`FAILED` 或 `DEGRADED`；
- Deployment 的 desired/observed state 和健康；
- Gateway/Auth 管理接口报告的 Endpoint、Link 健康；
- runtime 或 provider 的 missing/changed/unexpected/unreachable drift；
- 最后一次 Operation 和观测时间。

Web 和 TUI 只能使用正式 Status 展示健康，不能从 Spec 的 Endpoint/Link 字段推断运行状态。

## v1 API

正式资源位于 `/api/v1/topologies`：

- `POST /api/v1/topologies`：创建初始 draft revision，返回 `201` 和 ETag；
- `POST /api/v1/topologies/{id}/revisions`：以 `If-Match` 创建下一 draft revision；
- `PUT|DELETE /api/v1/topologies/{id}/draft/endpoints/{endpointId}`：编辑 draft Endpoint，每次成功都创建新 revision；
- `PUT|DELETE /api/v1/topologies/{id}/draft/links/{sourceEndpoint}/{targetEndpoint}`：编辑 draft Link，每次成功都创建新 revision；
- `POST /api/v1/topologies/{id}:validate`：校验 schema 和已注册服务引用；
- `POST /api/v1/topologies/{id}:diff`：比较指定 revision，未指定时比较 applied head 与 draft head；
- `POST /api/v1/topologies/{id}:apply`：返回 `202 + operation_id`；
- `POST /api/v1/topologies/{id}:rollback`：创建新的 rollback revision 并返回 `202 + operation_id`；
- `GET /api/v1/topologies/{id}/status`：读取实时 Status；
- 集合和 revision 历史使用稳定 cursor 分页。

所有 mutation 要求 `Idempotency-Key`。成功响应包含 `request_id`，失败使用 `application/problem+json`。

## Apply saga

apply 在一个短事务中取得 draft 的 apply ownership、持久化 Operation 和 Job，然后在事务外执行 provider I/O：

1. Gateway apply；
2. Auth apply；
3. 两个 provider 都返回同步、身份匹配且内容 hash 匹配的确认后，推进 applied head；
4. Auth 拒绝时恢复 Gateway 的上一 revision；首次 apply 则删除刚创建的 Gateway 投影；
5. provider 结果不确定时，先补偿该 provider，再补偿已经成功的步骤；
6. 补偿成功记为 `FAILED`，applied head 保持在最后已证明的 revision；补偿失败记为 `DEGRADED/NEEDS_ATTENTION`。

缺少 Gateway 或 Auth provider 时，plan/apply 在外部副作用前拒绝，不返回 deferred 或假成功。

## 崩溃恢复与对账

安全的 `QUEUED` apply Job 可在进程重新打开同一 SQLite/PostgreSQL 数据库后继续执行。若控制面在 provider 调用期间崩溃并导致 lease 过期，结果不可证明：Job 和 Operation 进入 `NEEDS_ATTENTION`，不得盲目重放。apply ownership 会以 `DEGRADED` 结束，保留最后已证明的 applied head，并允许 reconciler 继续读取 provider 的实际投影。

Reconciler 独立读取 Gateway、Auth 和 runtime 状态。provider 不可达、revision/hash 不匹配、额外 Endpoint/Link、Deployment desired/observed 不一致都会形成显式 drift。观测结果通过 applied revision CAS 写入，旧观测不能覆盖新 apply 的 `RECONCILING` 状态。

## Desktop 持久化

Desktop 默认使用应用数据目录中的 SQLite。revision、heads、Status、Operation 和 Job 均写入同一持久数据库；重启不依赖内存镜像。布局保存在按用户和 topology 隔离的 UI state 中，不属于 TopologySpec。

## 上线门禁

Topology v1 的定向门禁覆盖：完整 draft→revision→validate→diff→apply→status→rollback→reapply 流程、并发 revision CAS、陈旧 ETag、provider 失败补偿、补偿失败 Degraded、直接漂移观测、排队任务重启恢复，以及未知 provider 结果的 `NEEDS_ATTENTION` 恢复。
