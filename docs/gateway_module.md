# OJOS Gateway 模块开发文档

## 一、模块定位

`services/gateway` 是 OJOS 当前的 HTTP 网关服务。

它是第一个完整接入 `services/shared` 的 Go 微服务，负责验证整个基础设施链路：

```text
go-zero HTTP Server
配置加载
结构化日志
PostgreSQL 连接池
NATS EventBus
OpenTelemetry + Jaeger
Recovery Middleware
HTTP Logging Middleware
统一 JSON 响应
Docker Compose 部署
```

当前 Gateway 处于 MVP 阶段，主要职责是：

```text
提供统一 HTTP 入口
接入公共中间件
完成可观测性链路验证
提供 /health 健康检查
为后续 Auth/User/Problem/Contest 等服务提供网关基础
```

---

# 二、目录结构

当前 Gateway 目录结构如下：

```text
services/gateway/

├── configs/
│   └── config.yaml
│
├── internal/
│   ├── app/
│   │   └── app.go
│   │
│   ├── handler/
│   │   └── health.go
│   │
│   └── router/
│       └── router.go
│
├── Dockerfile
├── go.mod
├── go.sum
└── main.go
```

---

# 三、配置文件

Gateway 配置文件位于：

```text
services/gateway/configs/config.yaml
```

示例：

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

说明：

```text
service.name
```

用于服务名、日志字段、Jaeger service name。

```text
service.port
```

用于 Gateway HTTP 端口。

```text
database.url
```

用于连接 PostgreSQL。

```text
jaeger.endpoint
```

用于 OpenTelemetry OTLP gRPC exporter。

```text
nats.url
```

用于 NATS EventBus。

---

# 四、App 初始化流程

Gateway 的核心初始化逻辑位于：

```text
services/gateway/internal/app/app.go
```

`App` 结构体保存服务运行所需的基础设施对象：

```go
type App struct {
    Cfg      *config.Config
    Logger   *zap.Logger
    DB       *pgxpool.Pool
    Tracer   *sdktrace.TracerProvider
    EventBus *events.Bus
}
```

初始化流程：

```text
加载配置
    ↓
初始化 logger
    ↓
初始化 tracing
    ↓
初始化 PostgreSQL 连接池
    ↓
初始化 NATS EventBus
    ↓
创建 gateway.startup span
    ↓
返回 App
```

这样做的好处是：

```text
main.go 不直接关心基础设施细节
router/handler/middleware 可以通过 App 复用公共资源
服务关闭时可以统一释放资源
```

---

# 五、main.go 启动流程

Gateway 入口位于：

```text
services/gateway/main.go
```

启动流程：

```text
创建 context
    ↓
app.New(ctx)
    ↓
创建 go-zero rest.Server
    ↓
注册 Recovery Middleware
    ↓
注册 Logging / Tracing Middleware
    ↓
注册 Router
    ↓
输出 gateway listening 日志
    ↓
启动 server
    ↓
监听 SIGINT / SIGTERM
    ↓
server.Stop()
    ↓
app.Close()
```

当前使用：

```text
go-zero/rest
```

作为 HTTP server 框架。

---

# 六、中间件接入

Gateway 当前注册两个中间件：

```text
Recovery
Logging + Tracing
```

## 6.1 Recovery

注册方式：

```go
server.Use(func(next http.HandlerFunc) http.HandlerFunc {
    return sharedmw.Recovery(a.Logger, next)
})
```

作用：

```text
捕获 panic
记录错误日志
返回 500
防止服务异常崩溃
```

## 6.2 Logging + Tracing

注册方式：

```go
server.Use(a.LoggingMiddleware)
```

其中：

```go
func (a *App) LoggingMiddleware(next http.HandlerFunc) http.HandlerFunc {
    return sharedmw.Logging(a.Logger, a.Tracer, next)
}
```

该中间件负责：

```text
创建 HTTP server span
记录 HTTP 请求日志
注入 trace_id / span_id
上报 Jaeger
记录状态码和耗时
```

当前 HTTP tracing 使用：

```text
otelhttp.NewHandler
```

而不是手写 span。

---

# 七、Router 路由层

Gateway 路由集中注册在：

```text
services/gateway/internal/router/router.go
```

当前注册：

```text
GET /health
```

示例结构：

```go
func Register(server *rest.Server, a *app.App) {
    server.AddRoute(rest.Route{
        Method:  http.MethodGet,
        Path:    "/health",
        Handler: handler.Health(a.EventBus),
    })
}
```

后续新增路由时，应优先在 router 层集中注册，避免把路由全部堆进 `main.go`。

未来可以扩展为：

```text
router/
├── health.go
├── auth.go
├── user.go
├── problem.go
├── contest.go
└── submission.go
```

---

# 八、Health Handler

Gateway 当前健康检查接口：

```text
GET /health
```

位于：

```text
services/gateway/internal/handler/health.go
```

返回：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "status": "ok"
  }
}
```

同时会发布 NATS 事件：

```text
gateway.health.checked
```

用于验证 EventBus 是否可用。

Handler 当前只负责业务逻辑：

```text
发布 health 事件
返回统一 JSON 响应
```

不在 handler 内手写 HTTP span。

HTTP span 统一由 middleware 负责。

---

# 九、可观测性

Gateway 当前已经打通完整可观测链路：

```text
HTTP Request
    ↓
go-zero router
    ↓
Recovery Middleware
    ↓
Logging + otelhttp Middleware
    ↓
Handler
    ↓
Zap Log
    ↓
OpenTelemetry
    ↓
Jaeger
```

当前已验证 Jaeger 中存在：

```text
gateway-service: gateway.startup
gateway-service: GET /health
```

Gateway 请求日志示例：

```json
{
  "level": "info",
  "msg": "http request",
  "service": "gateway-service",
  "trace_id": "...",
  "span_id": "...",
  "method": "GET",
  "path": "/health",
  "status": 200,
  "duration": 0.000309788
}
```

---

# 十、Dockerfile

Gateway 的 Dockerfile 位于：

```text
services/gateway/Dockerfile
```

核心逻辑：

```text
使用 golang:1.26.3
设置 WORKDIR /app
复制 gateway/go.mod gateway/go.sum
复制 shared/go.mod shared/go.sum
go mod download
复制 gateway 源码
复制 shared 源码
go build -o gateway .
CMD ["./gateway"]
```

由于 Gateway 依赖 shared，所以 Docker build context 必须能访问：

```text
services/gateway
services/shared
```

---

# 十一、Docker Compose 集成

在 Docker Compose 中，Gateway build 配置应使用：

```yaml
gateway:
  build:
    context: ../../services
    dockerfile: gateway/Dockerfile
  depends_on:
    postgres:
      condition: service_healthy
    nats:
      condition: service_started
    jaeger:
      condition: service_started
  ports:
    - "8080:8080"
```

不能写成：

```yaml
build: ../../services/gateway
```

否则 Docker 构建时无法访问 `services/shared`。

---

# 十二、当前验收结果

当前已经验证：

```powershell
docker compose up -d --build
```

容器状态：

```text
PostgreSQL Healthy
Redis Running
NATS Running
Jaeger Running
Gateway Running
Auth Running
```

Gateway health：

```powershell
curl http://localhost:8080/health
```

返回：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "status": "ok"
  }
}
```

Gateway 日志中出现：

```text
gateway listening
http request
trace_id
span_id
method=GET
path=/health
status=200
```

Jaeger 中出现：

```text
gateway-service: gateway.startup
gateway-service: GET /health
```

---

# 十三、已解决的问题记录

## 13.1 Docker 容器名冲突

曾出现：

```text
Conflict. The container name "/compose-gateway-1" is already in use
```

解决方式：

```powershell
docker compose down
docker rm -f compose-gateway-1
docker compose up -d --build
```

## 13.2 Docker build context 问题

Gateway 依赖 shared，因此 Compose build context 必须是：

```text
../../services
```

## 13.3 配置文件路径问题

服务启动时曾出现：

```text
Config File "configs" Not Found in "[/app/gateway/configs]"
```

正确约定：

```text
configs/config.yaml
```

Viper 中应读取：

```go
v.SetConfigName("config")
v.SetConfigType("yaml")
v.AddConfigPath("./configs")
```

## 13.4 Jaeger 只有 startup 没有 HTTP span

曾出现：

```text
Jaeger 中只有 gateway.startup
没有 GET /health
```

最终原因：

```text
手写 HTTP span 没有稳定接入 go-zero HTTP 请求链路
```

最终解决方式：

```text
使用 otelhttp.NewHandler
显式传入当前 TracerProvider
设置 SpanNameFormatter
```

关键点：

```go
otelhttp.WithTracerProvider(tp)
```

---

# 十四、当前完成状态

Gateway MVP 当前完成：

```text
go-zero rest server      ✅
配置加载                 ✅
结构化日志               ✅
PostgreSQL 连接池         ✅
NATS EventBus            ✅
OpenTelemetry tracing    ✅
Jaeger 可观测             ✅
Recovery middleware      ✅
HTTP logging middleware  ✅
统一 JSON 响应            ✅
Router 层                 ✅
Health handler            ✅
Docker Compose            ✅
Graceful shutdown         ✅
```

---

# 十五、后续计划

Gateway 后续可继续扩展：

```text
JWT Middleware
RBAC Middleware
CORS Middleware
Request ID Middleware
Auth Service 转发
User Service 转发
Problem API 聚合
Contest API 聚合
Submission API 聚合
Prometheus Metrics
限流 / 熔断 / 超时
```

当前阶段 Gateway MVP 已经完成，可以支撑后续 Auth Service 业务开发。
