# Operation 模型

Operation 是编排变更与观测的审计单元。

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

计划阶段持久化 `operation_id`、`action`、`target_type`、`target_id`、`plan`、`status`、`created_at`
和 `updated_at`。确认阶段写入 `AWAITING_CONFIRMATION`。应用阶段获取 operation 锁、进入 `RUNNING`、写入
步骤日志，然后写入结果或错误状态。回滚阶段把原 operation 标记为 `ROLLED_BACK` 并写入回滚日志。

执行器只支持固定 action。任意 shell、任意脚本路径、用户提供的命令字符串和远程 root shell 都在模型之外。

当前固定 driver 为：

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

GUI、TUI 和 daemon 使用同一个 dispatcher 和 store-backed operation 状态机。它们不能绕过 core 去改动状态
或运行 action。
