# Operation 与 Job 模型

Operation 是一次用户可见、可审计、可恢复的长操作聚合；Job 是 Operation 分派给指定
Node Agent 或控制面内部执行器的持久步骤。Store、Topology、Deployment 与 Node 的异步
mutation 都返回 `202 + operation_id`，真实完成状态只能从 Operation/Job 投影和事件确认。

## Operation 状态

```text
PLANNED -> CONFIRMED -> ENQUEUING -> RUNNING -> SUCCEEDED
    |          |             |          |        FAILED
    |          |             |          |        NEEDS_ATTENTION
    +----------+-------------+----------+------> CANCELLED
                                           \
                                            -> CANCELLING -> CANCELLED

SUCCEEDED/FAILED -- rollback --> 新的 ROLLBACK Operation -> ROLLED_BACK 或 FAILED
FAILED -- retry --> ENQUEUING（新 generation，仅重建可证明失败的步骤）
```

正式状态为 `PLANNED`、`CONFIRMED`、`ENQUEUING`、`RUNNING`、`CANCELLING`、
`SUCCEEDED`、`FAILED`、`CANCELLED`、`NEEDS_ATTENTION` 与 `ROLLED_BACK`。

- plan 持久化 action、target、规范化请求、不可变计划摘要、依赖 DAG 和补偿步骤；重放同一
  operation/idempotency key 必须得到同一结果，变更 payload 会冲突。
- confirm 只接受 `PLANNED`；apply 将已确认计划置为 `ENQUEUING`，幂等地物化 Job 后才进入
  `RUNNING`。
- cancel 在尚未 claim 时立即取消；已 lease 的 Job 进入协作式取消，Operation 为
  `CANCELLING`，直到所有结果可证明。
- retry 只允许 `FAILED`，使用新的 generation 重建失败步骤；`NEEDS_ATTENTION` 在人工对账前
  禁止 retry。
- rollback 不修改旧 Operation。它从可回滚的 `SUCCEEDED`/`FAILED` Operation 创建新的
  rollback Operation，并保留来源 ID 和反向计划。

## Job 状态与 lease

```text
QUEUED -> LEASED -> SUCCEEDED
                 -> RETRY_WAIT -> LEASED
                 -> FAILED
                 -> CANCEL_REQUESTED -> CANCELLED
                 -> NEEDS_ATTENTION
```

默认 lease 30 秒、heartbeat 10 秒、长轮询 25 秒、最多 3 次尝试，退避为 1/5/30 秒。
claim 使用数据库 CAS，同一 Job 在并发 claim 中只能产生一个有效 lease。heartbeat、事件和
complete 都校验 lease token；旧 lease、重复完成和乱序事件不会改变已确认结果。

投递语义是至少一次。每个副作用必须使用稳定的 Job idempotency key；Node 在本地 SQLite
ledger 记录 claim、attempt 与副作用结果。lease 过期时：

- 可证明没有发生副作用，且仍有预算时，进入 `RETRY_WAIT`；
- 已耗尽重试预算，或取消期间结果未知，进入 `NEEDS_ATTENTION`；
- 对不可证明的 mutation 结果绝不盲目重跑，也不自动执行补偿。

## 依赖、补偿与恢复

计划步骤声明依赖关系和 `ON_SUCCESS`/`ON_FAILURE` 条件。依赖失败时，后续正常步骤标记为
skipped；只有失败结果已知时才物化补偿 Job。任何步骤进入 `NEEDS_ATTENTION` 时自动补偿停止，
由运维人员结合 Node ledger、Docker RepoDigest、Provider 状态和审计记录对账。

控制面在监听端口前恢复过期 lease、`ENQUEUING`、`RUNNING` 和 `CANCELLING` Operation。
部分 enqueue 后崩溃会按稳定 step/generation key 补齐，不会创建第二个副作用 Job。SIGTERM
先停止接收新工作，最多排空 30 秒；未完成工作由下次启动按上述规则恢复。

## API 与审计

- 所有 mutation 要求 `Idempotency-Key`，并在外部副作用前写入 append-only audit intent；
  审计写入失败时不开始副作用。
- `GET /api/v1/operations/{id}` 读取聚合；日志接口读取有序持久日志；事件接口使用 SSE，支持
  `Last-Event-ID` 重连与事件去重。
- Operation/Job 资源本身长期可查询。`ORCHESTRATOR_LOG_RETENTION_DAYS` 只清理已终结
  Operation 的详细日志、已终结 Job 的事件和过期幂等响应，不清理资源、Topology 历史或审计。
- `operation.create/update/delete`、日志修改/删除、诊断修改/删除和伪 service enable/disable
  不属于 v1 产品 action；GA 路由不会用通用 CRUD 或 metadata-only 成功代替真实执行。

Node v1 只使用带 SPIFFE Node ID 的 mTLS pull 协议进行 claim/heartbeat/complete/cancel。
0.2 的 Node push、共享 bearer 和 shell 拼接路径不属于 v1 Operation/Job 模型。
