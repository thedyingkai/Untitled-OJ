# Endpoint、Link 与 ApiBinding 规范

Topology Spec 只描述期望关系；真实健康、路由、凭据与 drift 由 `TopologyStatus` 和持久 `ApiBinding` 表达。Endpoint/Link 编辑先创建不可变 draft revision，只有 apply 成功才改变业务数据面。

## Endpoint

Endpoint 引用已经注册或部署的 Service。正式期望字段为：

```text
endpoint
service_id
protocol
health_path
display_name
note
config
```

运行时地址必须来自 RuntimeInstance/Node facts，不能由业务 manifest 或管理员 label 伪造。健康、reachable、latency、container ID、Operation、日志和画布坐标不写入 Spec。

## Link

Link 连接 source consumer Endpoint 与 target provider Endpoint：

```yaml
source_endpoint: judge-worker-b
target_endpoint: judge-api-a
protocol: https
auth_mode: workload
scope: topology
enabled: true
api_bindings:
  - requirement: judge_control
    api_id: judge.worker.control
    version: 1.0.0
    provider_deployment_id: deployment-judge-api-a
    selection: explicit
```

`api_bindings` 中的 requirement 必须存在于 source Deployment 的签名 Release v2，API/version 必须由 target Deployment 的精确 Release 提供。一个 consumer Deployment 的同一 requirement 最终只能有一个活动 Binding。零候选、版本不兼容、陈旧 RuntimeReport 或未确认的多候选都会使 validate/apply 在外部副作用前失败。

停用或删除 Link 必须创建新 revision 并 apply；不能直接修改运行路由。成功 apply 会提升 consumer 的 credential/context generation，使旧 JWT 即使尚未到期也立即失效。

## ApiBinding 运行投影

持久 `ApiBinding` 至少记录：

- binding ID、requirement、API/version；
- consumer/provider Deployment、Service、Node 和 Endpoint；
- Topology/revision/Link；
- provider path、Gateway virtual endpoint、methods、auth、permission 和 timeout；
- credential/context generation；
- desired/observed state、health、drift、reason 与最后 Operation。

Gateway 以 consumer Deployment + API ID 查找活动 Binding，并从已验证 JWT 推导 caller 身份；客户端提交的 caller service/node header 会被清理，不能参与授权。

## Apply、状态与升级

Topology apply 先暂存 Binding、Gateway 路由和 Auth grant，再让 Agent 原子物化 context/credential。consumer 健康后才激活 Binding 并推进 applied head；失败时恢复上一 generation 和投影，补偿不确定时进入 `DEGRADED/NEEDS_ATTENTION`。

provider 升级先启动并验证新 RuntimeInstance，再原子切换 Binding；consumer 不需要重启。外部修改路由、container digest、HostConfig 或 Binding 会由 reconciler 写入 Status drift。

正式 Schema 见 `platform/schemas/orchestrator/openapi-v1.yaml` 与 `api-binding-v1.schema.json`，完整执行语义见 [Topology 模型](../orchestrator/topology-model.md)。
