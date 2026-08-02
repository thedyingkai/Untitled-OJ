# Operation 模型

Operation 是一次编排动作的计划、执行和审计记录。

Operation 面向正式对象，如 service releases、services、endpoints、links、topology、log views 和
diagnostic reports。部署模板不是 operation 目标。

状态机为：

```text
PLANNED
AWAITING_CONFIRMATION
RUNNING
SUCCEEDED
FAILED
ROLLED_BACK
CANCELLED
EXPIRED
```

计划阶段持久化 `operation_id`、`action`、`target_type`、`target_id`、请求、计划、回滚计划和时间戳。
需要确认的动作由 `PLANNED` 进入 `AWAITING_CONFIRMATION`；这个名称表示已经确认、正在等待执行。应用阶段
获取 OperationLock，进入 `RUNNING`，逐步写日志，最后落到 `SUCCEEDED` 或 `FAILED`。

回滚读取原 Operation 中保存的前态，而不是机械反转当前值。Link 启停恢复原来的 `enabled`；Service 和 Host
生命周期恢复受影响的 `HostService`、`DeployedServiceApi` 和运行状态。生命周期应用和回滚都只有在请求显式
设置 `execute_service_driver=true` 时才会执行本地进程或 Compose 驱动。驱动或路由刷新失败时返回
`FAILED`，不会把 Operation 标成 `ROLLED_BACK`。

回滚入口还要满足两个条件：原 Operation 的状态是 `SUCCEEDED` 或 `FAILED`，并且
`rollback_plan.steps` 非空。空计划会在加锁、写回滚日志或恢复任何 store 对象之前直接阻止。即使计划里写了步骤，
执行器没有对应 mutation 的未知动作仍会失败；apply 也遵循同一规则，不会用伪造的 changed object 冒充成功。

`service.start/stop/restart/delete`、`host.start/stop` 以及已有 `service.enable/disable` 记录的回滚，每次都要重新传
`execute_service_driver=true`。授权检查先于 store 恢复，避免 `service.delete` 先恢复部分记录、随后才因驱动未授权
失败。`operation.create` 目前没有独立的真实 mutation，因此能力状态是 `UNSUPPORTED`。

`release.install` 可以不授权 driver，只登记或延后启动。若在 `operation.apply` 时才授予
`execute_service_driver=true`，dispatcher 会把这次授权写回原 Operation；后续回滚据此要求新的逐次授权，
不会把实际启动过的运行时误当成纯元数据安装。Node 端还必须打开
`ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER`，这个环境上限不能代替请求授权。
请求已设置 `execute_service_driver=true`、而目标 Node 没打开该上限时，安装会以 `FAILED` / `Blocked` 结束，
不会降级成 metadata-only。只有未授权 driver 的请求可以只登记元数据。
Node 真执行还要求专用 bearer、控制面内部 token 和 `ORCHESTRATOR_NODE_HOST_IP`；请求主机与 Endpoint host
必须和节点身份完全一致。

`external_service_running=true` 只用于登记已经由外部系统启动、且健康检查可达的 Endpoint。它与 driver 授权
互斥，也会跳过 Node 派发。登记前执行器拒绝覆盖可能仍活动的旧运行时；登记后写入
`runtime_owner=external`，控制面生命周期和 `service.delete` 不会接管该进程。运维人员应先在真实 owner 一侧
停止或移除运行时。

`release.rollback` 可以显式指定 `target_operation_id`。执行器会核对目标是否为同一 Service、同一指定版本且
状态允许回滚的 `release.install`。未指定目标时，它按 `created_at`、`updated_at` 和 Operation ID 的稳定顺序，
选择最新的成功安装，不依赖 store 的返回顺序。这个 wrapper 本身没有反向回滚计划；需要反向操作时应新建
install 或 rollback，不能再次回滚 wrapper。

`release.delete` 只删除 Release 记录。被 `HostService` 或 `DeployedServiceApi` 引用的版本会被拒绝；应先安装
其它版本，或用 `service.delete` 卸载 Service。删除历史版本不会停止当前进程，也不会清空当前路由和资源登记。
`service.delete` 只卸载由本控制面本地 driver 管理的部署；`runtime_owner=node` 或 `external` 时会拒绝执行。

执行器只支持固定 action 和发布契约声明的运行方式。任意 shell、任意脚本路径、用户提交的命令字符串和远程
root shell 都不在模型内。

当前固定 driver 为：

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

Web UI、TUI 和 daemon 使用同一个 dispatcher 与 store-backed 状态机。

## 回滚边界

- 数据库 schema 没有自动 down migration；需要从备份恢复。
- `release.install` 能恢复编排器中的 Service、Release、Endpoint、Link、API surface、路由和资源登记。获得
  驱动授权后，回滚会先停止当前固定运行时，再按快照恢复旧版本的 running/stopped 状态；无法安全表达的混合
  状态会明确阻塞。外部 Redis、存储和 auth-service 已产生的副作用不保证自动撤销。
- 当前 beta 会阻塞 `runtime_owner=node` 的升级、回滚以及 Service/Host 生命周期；远端 stop/rollback 协议尚未接通。运维人员应先核对真实运行态，再决定人工恢复，不能把控制面记录当成节点已回滚的证明。
