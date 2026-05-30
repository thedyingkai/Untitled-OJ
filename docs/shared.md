# OJOS Shared 模块开发文档

## 一、模块定位

`services/shared` 是 OJOS Go 微服务体系中的公共基础设施 SDK。

它本身不是一个独立运行的服务，不监听端口，也不直接被 Docker Compose 单独启动。

它的作用是为所有 Go 微服务提供统一的基础能力，包括：

```text
配置加载
结构化日志
数据库连接池
链路追踪
HTTP 中间件
事件总线
统一响应格式
```

当前已经接入 shared 的服务包括：

```text
services/gateway
services/auth
```

后续服务也应直接复用 shared，例如：

```text
services/user
services/problem
services/contest
services/submission
services/judge-api
```

---

# 二、目录结构

当前 shared 模块结构如下：

```text
services/shared/

├── config/
│   ├── config.go
│   └── load.go
│
├── database/
│   └── postgres.go
│
├── events/
│   ├── event.go
│   └── nats.go
│
├── logger/
│   └── logger.go
│
├── middleware/
│   ├── logging.go
│   └── recovery.go
│
├── response/
│   └── response.go
│
├── tracing/
│   └── tracing.go
│
├── go.mod
└── go.sum
```

---

# 三、config 配置模块

## 3.1 作用

`config` 模块负责统一加载服务配置。

当前使用：

```text
Viper
```

每个服务都需要在自己的服务目录下提供：

```text
configs/config.yaml
```

例如：

```text
services/gateway/configs/config.yaml
services/auth/configs/config.yaml
```

## 3.2 配置结构

当前统一配置结构包括：

```yaml
service:
  name: gateway-service
  port: 8080

database:
  url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

jaeger:
  endpoint: ojos-jaeger:4317

nats:
  url: nats://ojos-nats:4222
```

其中：

```text
service.name
```

用于日志服务名、Jaeger service name 等。

```text
service.port
```

用于 go-zero HTTP Server 监听端口。

```text
database.url
```

用于 PostgreSQL 连接池。

```text
jaeger.endpoint
```

用于 OpenTelemetry OTLP gRPC exporter。

```text
nats.url
```

用于 NATS EventBus。

## 3.3 注意事项

在 Docker 容器内部，数据库地址必须写：

```text
postgres:5432
```

不能写：

```text
localhost:5433
```

因为容器内的 `localhost` 指的是当前容器本身，而不是宿主机。

---

# 四、logger 日志模块

## 4.1 作用

`logger` 模块负责创建统一的结构化日志器。

当前使用：

```text
Zap
```

每条日志默认带有：

```text
service
```

字段。

## 4.2 trace 日志注入

`logger.WithTrace(ctx, log)` 会从请求上下文中读取：

```text
trace_id
span_id
```

并写入日志。

示例日志：

```json
{
  "level": "info",
  "msg": "http request",
  "service": "gateway-service",
  "trace_id": "56af8e5d99c1e0f39afcc2f144f63101",
  "span_id": "aa96fb71d55bb95f",
  "method": "GET",
  "path": "/health",
  "status": 200,
  "duration": 0.000309788
}
```

## 4.3 重要说明

`logger.WithTrace` 只负责读取 trace 信息并写入日志。

它不负责：

```text
创建 span
结束 span
导出 span
上报 Jaeger
```

HTTP 请求的 tracing 由 `middleware` 模块中的 OpenTelemetry HTTP instrumentation 负责。

---

# 五、database 数据库模块

## 5.1 作用

`database` 模块负责统一创建 PostgreSQL 连接池。

当前使用：

```text
pgxpool
```

## 5.2 当前能力

`database.NewPostgresPool(ctx, cfg)` 完成：

```text
读取 database.url
解析 pgxpool 配置
设置连接池参数
创建连接池
Ping 检查
返回 *pgxpool.Pool
```

## 5.3 连接池管理

连接池由每个服务的 `App` 持有，并在服务关闭时统一释放。

典型使用方式：

```go
pool, err := database.NewPostgresPool(ctx, cfg)
if err != nil {
    return nil, err
}
```

关闭方式：

```go
pool.Close()
```

---

# 六、tracing 链路追踪模块

## 6.1 作用

`tracing` 模块负责初始化 OpenTelemetry TracerProvider。

当前链路为：

```text
OpenTelemetry SDK
    ↓
OTLP gRPC Exporter
    ↓
Jaeger Collector
    ↓
Jaeger UI
```

## 6.2 Jaeger 配置

Docker Compose 中 Jaeger 需要开启 OTLP：

```yaml
jaeger:
  image: jaegertracing/all-in-one:latest
  container_name: ojos-jaeger
  environment:
    COLLECTOR_OTLP_ENABLED: "true"
  ports:
    - "16686:16686"
    - "4317:4317"
    - "4318:4318"
    - "14268:14268"
```

服务配置中使用：

```yaml
jaeger:
  endpoint: ojos-jaeger:4317
```

## 6.3 当前能力

每个服务启动时调用：

```go
tp, err := tracing.Init(ctx, cfg)
```

初始化成功后，服务会在 Jaeger 中显示为：

```text
gateway-service
auth-service
```

## 6.4 重要经验

开发过程中曾出现：

```text
日志中有 trace_id / span_id
但 Jaeger 中只有 gateway.startup
没有 GET /health
```

最终确认原因是：手写 HTTP span 没有稳定接入 go-zero 的 HTTP 请求链路。

最终解决方式是：

```text
HTTP 请求 tracing 统一交给 otelhttp.NewHandler
并显式传入当前 TracerProvider
```

关键代码：

```go
otelhttp.WithTracerProvider(tp)
```

以及：

```go
otelhttp.WithSpanNameFormatter(func(operation string, r *http.Request) string {
    return r.Method + " " + r.URL.Path
})
```

当前原则：

```text
tracing 包只初始化 TracerProvider
middleware 负责 HTTP trace
logger 只读取 trace_id / span_id
handler 不手写 HTTP span
```

---

# 七、middleware 中间件模块

## 7.1 Recovery Middleware

`Recovery` 中间件用于捕获 handler 中的 panic，避免服务直接崩溃。

作用：

```text
捕获 panic
记录错误日志
返回 500 响应
```

注册方式：

```go
server.Use(func(next http.HandlerFunc) http.HandlerFunc {
    return sharedmw.Recovery(a.Logger, next)
})
```

## 7.2 Logging + HTTP Tracing Middleware

该中间件负责：

```text
记录 HTTP 请求日志
创建 HTTP server span
向 Jaeger 上报请求 trace
将 trace_id / span_id 注入日志
记录 method / path / status / duration
```

当前实现基于：

```text
otelhttp.NewHandler
```

而不是手写：

```go
tracer.Start(...)
```

这样可以保证 HTTP 请求被正确识别为 server span，并在 Jaeger Operation 中显示，例如：

```text
GET /health
```

---

# 八、events 事件模块

## 8.1 作用

`events` 模块封装 NATS EventBus。

当前使用：

```text
NATS
```

## 8.2 事件结构

事件基础字段包括：

```text
id
type
producer
timestamp
payload
```

## 8.3 当前能力

提供：

```go
events.NewBus(cfg)
bus.Publish(ctx, subject, eventType, payload)
bus.Close()
```

## 8.4 当前已验证事件

Gateway health 会发布：

```text
gateway.health.checked
```

Auth health 会发布：

```text
auth.health.checked
```

后续业务事件可以继续扩展：

```text
user.registered
user.login
submission.created
submission.finished
contest.started
contest.ended
```

---

# 九、response 响应模块

## 9.1 作用

`response` 模块负责统一 HTTP JSON 返回格式。

## 9.2 成功响应

```json
{
  "code": 0,
  "msg": "success",
  "data": {}
}
```

## 9.3 错误响应

```json
{
  "code": 10001,
  "msg": "error message"
}
```

## 9.4 使用方式

```go
response.Success(w, data)
response.Error(w, code, msg)
```

## 9.5 示例

Gateway health：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "status": "ok"
  }
}
```

Auth health：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "service": "auth",
    "status": "ok"
  }
}
```

---

# 十、Shared 当前完成状态

当前 shared 已经完成：

```text
config      ✅
logger      ✅
database    ✅
tracing     ✅
events      ✅
middleware  ✅
response    ✅
```

可以支撑后续微服务开发。

---

# 十一、后续可扩展方向

后续 shared 可以继续加入：

```text
JWT 工具
RBAC 中间件
Request ID 中间件
CORS 中间件
统一错误码
统一 validator
Redis client
gRPC client/server helper
Prometheus metrics
```

当前阶段 shared MVP 已经完成，可以稳定复用于 Gateway、Auth 以及后续服务。
