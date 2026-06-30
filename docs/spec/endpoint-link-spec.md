# Endpoint / Link 规范

Endpoint 是运行时唯一连接身份，格式固定为：

```text
ip:port:service-name
```

Endpoint 直接绑定 `service_id`，不通过额外运行实例对象包装。

Endpoint 字段包括：

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

Endpoint health 规则：

- `http`：GET `health_path`，2xx/3xx 为 healthy，连接失败为 unreachable，非成功状态为 degraded。
- `https`：当前执行 TCP 连接级检查；TLS 和 HTTP status 检查尚未作为正式能力声明。
- `tcp`：能建立 TCP 连接为 healthy，失败为 unreachable。
- `postgresql` / `redis`：当前至少执行 TCP 级检查。

Link 是 Endpoint Pair：

```text
source_endpoint -> target_endpoint
```

Link 字段包括：

```text
source_endpoint
target_endpoint
protocol
auth_mode
scope
health
latency_ms
config_ref
secret_ref
policy
created_at
updated_at
```

Link health 至少检查 source endpoint 是否存在、target endpoint 是否存在、target 是否 reachable、protocol 是否匹配、auth_mode 与 scope 是否完整。可测得延迟时写入 `latency_ms`。

Gateway 只能读取 Orchestrator 输出的 Endpoint/Link routing snapshot 并代理业务流量；它不能写 Endpoint、Link 或 Topology。
