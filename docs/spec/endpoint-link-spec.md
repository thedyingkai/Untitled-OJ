# Endpoint 与 Link 规范

## Endpoint

Endpoint 是运行时连接身份，格式固定为：

```text
ip:port:service-name
```

`service_name` 必须与 `service_id` 相同。Endpoint 直接绑定 Service，不再套一层 machine、installation 或 instance 对象。

持久化字段如下：

```text
endpoint
service_id
protocol
health_path
health
reachable
display_name
note
config
created_at
updated_at
```

正式协议为 `http`、`https`、`tcp`、`postgres` 和 `redis`。健康检查规则：

- `http` / `https`：对 `health_path` 发 GET，不跟随重定向。2xx/3xx 记为 `healthy`，其它 HTTP status 记为 `degraded`，连接失败记为 `unreachable`。
- `tcp` / `postgres` / `redis`：当前只检查 TCP 能否建立连接。
- 没有 `health_path` 时，HTTP(S) 默认检查 `/`。

健康结果同时更新 `health` 与 `reachable`。可测得的耗时写入检查结果，但 Endpoint 本身不保存 `latency_ms`。

## Link

Link 的身份是一对已注册 Endpoint：

```text
source_endpoint -> target_endpoint
```

source 和 target 不能相同。字段如下：

```text
source_endpoint
target_endpoint
protocol
auth_mode
scope
enabled
health
latency_ms
config_ref
secret_ref
policy
created_at
updated_at
```

`enabled` 默认为 `true`。`link.update` 没有提交该字段时保留原值；`link.enable` 和 `link.disable` 在 Operation 中保存切换前状态，以便准确回滚。

自动健康检查会确认 source/target 是否存在、target 是否可达、协议族是否匹配，以及 `auth_mode`、`scope` 是否填写。结果可能是 `healthy`、`degraded`、`blocked` 或 `unreachable`，可测得的 target 延迟写入 `latency_ms`。

停用的 Link 不参加 reconciler 探测，也不计入诊断报告的 unhealthy 数量。旧健康值会保留作审计，`enabled=false` 才是当前生效状态。

## 路由和权限边界

Gateway 读取 Orchestrator 输出的 Endpoint、Link 与 effective API route，只代理业务流量；它不能写 Endpoint、Link 或 Topology。

Link 表达“谁可以连接谁”，API surface 再约束 visibility、`auth_mode` 和 permission。service bearer 只会转发到精确的 auth permission-check API，其它 provider 不接收调用方的 service credential。
