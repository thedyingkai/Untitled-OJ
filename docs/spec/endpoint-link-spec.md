# Endpoint / Link 规范

Endpoint 是运行时唯一连接身份，格式固定为：

```text
IP:Port
```

一个 Endpoint 直接代表一个运行中的 Service 入口，并直接绑定 `service_id`。同一个 Service 可以拥有多个 Endpoint。Orchestrator 不通过额外主机对象、设备对象或安装实例对象包装运行实例。

## Endpoint

Endpoint 至少包含：

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

Endpoint 由 Orchestrator 注册、更新、删除和检查健康。Service 只能在 `service.yaml` 中声明 `default_port`，不能决定最终 IP。

## Link

Link 是 Endpoint Pair：

```text
source endpoint -> target endpoint
```

Link 可附带：

```text
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

Link 由 Orchestrator 创建、更新、删除和检查健康。Service 只能声明需要哪些连接，不能自行修改全局 Link。

Link 是通信授权来源。`source endpoint -> target endpoint` 不存在时，Gateway 不能为该连接生成代理路由，也不能把请求转发到目标 Endpoint。Endpoint 不健康或不可达时，对应 route 必须进入 degraded 状态，不能被当成健康上游。

Gateway 只能根据 Orchestrator 输出的 Endpoint / Link routing snapshot 代理业务流量。Gateway 可以认证、鉴权、审计、限流和上报 health，但不能写 Endpoint、Link 或 Topology。

Endpoint 的 IP 部分可以用于展示某个 host 上的所有 Service、日志和状态：

```text
group by endpoint host/IP
```

这只是视图分组，不引入额外核心对象。
