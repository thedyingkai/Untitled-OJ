# OJOS Shared 模块开发文档

## 一、模块定位

`services/shared` 是 OJOS 平台的公共基础库，不是独立 HTTP 服务，也不应该被 `goctl api new` 生成。

Shared 的职责是为各个 Go 微服务提供稳定、通用、可复用的基础能力，包括：

```text
数据库连接
NATS 事件总线
结构化日志
链路追踪
HTTP 中间件
```

Shared 不负责业务逻辑，不保存业务配置，不依赖某一个具体服务。

当前 Shared 已经完成旧兼容模块清理，不再保留：

```text
shared/config
shared/response
```

当前 Shared 是一个纯公共库。

---

## 二、当前完成状态

当前 Shared 已完成：

```text
Go module 独立化
旧 config 模块删除
旧 response 模块删除
PostgreSQL URL 初始化
NATS EventBus 初始化
统一 Event 结构
zap logger
trace_id / span_id 日志注入
OpenTelemetry OTLP 初始化
go-zero Recovery 中间件适配
go-zero Logging 中间件适配
shared 自身 go build ./... 通过
gateway 接入通过
auth 接入通过
```

当前状态可以记为：

```text
Shared go-zero 适配 v0.2 完成
```

---

## 三、目录结构

当前目录结构：

```text
services/shared/

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
│   ├── gozero.go
│   ├── logging.go
│   └── recovery.go
│
├── tracing/
│   └── tracing.go
│
├── go.mod
└── go.sum
```

当前不应存在：

```text
services/shared/config
services/shared/response
```

---

## 四、go.mod

Shared 是独立 Go module。

模块名：

```go
module ojos-shared
```

其他服务使用 Shared 时，需要在自己的 `go.mod` 中加入：

```go
require ojos-shared v0.0.0

replace ojos-shared => ../shared
```

例如：

```go
module ojos-auth

go 1.26

require ojos-shared v0.0.0

replace ojos-shared => ../shared
```

---

## 五、database 模块

路径：

```text
services/shared/database/postgres.go
```

### 5.1 职责

`database` 模块负责创建 PostgreSQL 连接池。

当前只保留 URL 驱动的初始化方式，不再依赖旧的 shared/config。

### 5.2 推荐函数

```go
func NewPostgresPoolByURL(ctx context.Context, databaseURL string) (*pgxpool.Pool, error)
```

功能：

```text
解析 PostgreSQL URL
创建 pgxpool 连接池
Ping 数据库
连接失败时关闭连接池
返回可复用的 *pgxpool.Pool
```

示例：

```go
db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
if err != nil {
    log.Fatalf("connect postgres failed: %v", err)
}
```

### 5.3 当前约束

Shared 不再提供：

```go
NewPostgresPool(ctx, cfg)
```

服务必须自己在 `internal/config/config.go` 中定义数据库配置，然后把 URL 传给 Shared。

---

## 六、events 模块

路径：

```text
services/shared/events/
```

当前包含：

```text
event.go
nats.go
```

### 6.1 Event 结构

`event.go` 定义统一事件模型：

```go
type Event struct {
    ID        string          `json:"id"`
    Type      string          `json:"type"`
    Producer  string          `json:"producer"`
    Timestamp time.Time       `json:"timestamp"`
    Payload   json.RawMessage `json:"payload"`
}
```

字段说明：

| 字段          | 含义    |
| ----------- | ----- |
| `ID`        | 事件 ID |
| `Type`      | 事件类型  |
| `Producer`  | 事件生产者 |
| `Timestamp` | 事件时间  |
| `Payload`   | 业务载荷  |

推荐事件时间使用 UTC：

```go
Timestamp: time.Now().UTC()
```

### 6.2 Event 构造函数

```go
func New(eventType string, producer string, payload any) (*Event, error)
```

功能：

```text
生成 UUID
设置事件类型
设置生产者
设置当前 UTC 时间
序列化 payload
返回 Event
```

---

### 6.3 Bus 结构

`nats.go` 中定义：

```go
type Bus struct {
    conn     *nats.Conn
    producer string
}
```

其中：

| 字段         | 含义      |
| ---------- | ------- |
| `conn`     | NATS 连接 |
| `producer` | 当前服务名   |

### 6.4 创建 Bus

```go
func NewBusByURL(url string, producer string) (*Bus, error)
```

示例：

```go
bus, err := events.NewBusByURL(c.Nats.Url, c.Name)
if err != nil {
    log.Fatalf("connect nats failed: %v", err)
}
```

### 6.5 发布事件

```go
func (b *Bus) Publish(
    ctx context.Context,
    subject string,
    eventType string,
    payload map[string]any,
) error
```

示例：

```go
err := bus.Publish(
    ctx,
    "submission.created",
    "submission.created",
    map[string]any{
        "submission_id": id,
    },
)
```

事件最终结构类似：

```json
{
  "id": "uuid",
  "type": "submission.created",
  "producer": "judge-api-service",
  "timestamp": "2026-05-31T04:00:00Z",
  "payload": {
    "submission_id": 1
  }
}
```

### 6.6 关闭 Bus

```go
func (b *Bus) Close()
```

服务退出时应调用：

```go
if s.Bus != nil {
    s.Bus.Close()
}
```

### 6.7 当前限制

当前 events 基于 NATS Core Pub/Sub。

它适合普通事件广播，但不适合作为可靠任务队列。

对于 Judge 任务，后续需要补：

```text
worker 扫描 PENDING 任务
或 NATS JetStream
或 Redis Stream
或数据库任务队列
```

---

## 七、logger 模块

路径：

```text
services/shared/logger/logger.go
```

### 7.1 创建 Logger

```go
func New(service string) (*zap.Logger, error)
```

示例：

```go
zlog, err := logger.New(c.Name)
if err != nil {
    log.Fatalf("init logger failed: %v", err)
}
```

创建出的日志会包含服务名字段：

```json
{
  "service": "auth-service"
}
```

### 7.2 Trace 注入

```go
func WithTrace(ctx context.Context, log *zap.Logger) *zap.Logger
```

功能：

```text
从 context 中读取 OpenTelemetry SpanContext
如果存在合法 trace_id / span_id，则写入日志字段
否则返回原 logger
```

示例：

```go
logger.WithTrace(r.Context(), log).Info(
    "http request",
    zap.String("method", r.Method),
    zap.String("path", r.URL.Path),
)
```

---

## 八、tracing 模块

路径：

```text
services/shared/tracing/tracing.go
```

### 8.1 职责

`tracing` 模块负责初始化 OpenTelemetry，并通过 OTLP gRPC 上报到 Jaeger。

### 8.2 初始化函数

```go
func InitOTLP(
    ctx context.Context,
    serviceName string,
    endpoint string,
) (*sdktrace.TracerProvider, error)
```

示例：

```go
tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
if err != nil {
    log.Fatalf("init tracing failed: %v", err)
}
```

该函数负责：

```text
创建 OTLP gRPC exporter
设置 service.name
创建 sdktrace.TracerProvider
注册全局 TracerProvider
注册 TraceContext propagator
注册 Baggage propagator
```

### 8.3 Trace Context 传播

`InitOTLP` 应设置：

```go
otel.SetTextMapPropagator(
    propagation.NewCompositeTextMapPropagator(
        propagation.TraceContext{},
        propagation.Baggage{},
    ),
)
```

这样 Gateway 转发请求时可以把 trace 上下文注入 HTTP Header，下游服务可以继续同一条 trace。

### 8.4 当前限制

当前 tracing 使用 SimpleSpanProcessor。

后续可以改进为：

```text
BatchSpanProcessor
采样率配置
按环境开关 tracing
超时控制
失败降级
```

---

## 九、middleware 模块

路径：

```text
services/shared/middleware/
```

当前包含：

```text
recovery.go
logging.go
gozero.go
```

### 9.1 Recovery

底层 Recovery 函数用于捕获 panic。

典型签名：

```go
func Recovery(log *zap.Logger, next http.HandlerFunc) http.HandlerFunc
```

功能：

```text
recover panic
记录错误日志
返回 HTTP 500
避免服务进程崩溃
```

### 9.2 go-zero RecoveryMiddleware

```go
func RecoveryMiddleware(log *zap.Logger) func(http.HandlerFunc) http.HandlerFunc
```

用于 go-zero：

```go
server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
```

### 9.3 go-zero LoggingMiddleware

```go
func LoggingMiddleware(
    log *zap.Logger,
    tp *sdktrace.TracerProvider,
) func(http.HandlerFunc) http.HandlerFunc
```

用于 go-zero：

```go
server.Use(sharedmw.LoggingMiddleware(svcCtx.Logger, svcCtx.Tracer))
```

当前功能：

```text
创建 HTTP span
执行 handler
记录 method
记录 path
记录 duration
从 context 注入 trace_id / span_id 到日志
```

### 9.4 当前限制

当前 LoggingMiddleware 还可以增强：

```text
记录 HTTP status
记录 client_ip
记录 user_agent
记录 response_size
记录 request_id
支持慢请求日志
支持日志采样
```

---

## 十、Shared 与 go-zero 服务的关系

Shared 本身不是 go-zero 服务。

go-zero 服务通过自己的 `ServiceContext` 注入 Shared 能力。

示例：

```go
type ServiceContext struct {
    Config config.Config

    Logger *zap.Logger
    DB     *pgxpool.Pool
    Tracer *sdktrace.TracerProvider
    Bus    *events.Bus
}
```

初始化：

```go
zlog, _ := logger.New(c.Name)
tp, _ := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
db, _ := database.NewPostgresPoolByURL(ctx, c.Database.Url)
bus, _ := events.NewBusByURL(c.Nats.Url, c.Name)
```

每个服务自己维护自己的配置结构：

```go
type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Nats     NatsConfig
    Jaeger   JaegerConfig
}
```

Shared 不再主动加载配置。

---

## 十一、当前已接入服务

当前已接入 Shared 的服务：

```text
gateway
auth
```

### 11.1 Gateway

Gateway 使用：

```text
database
events
logger
middleware
tracing
```

已验证：

```text
GET /health
POST /api/auth/login
GET /api/auth/profile
GET /api/judge/submissions/:id/cases
```

### 11.2 Auth

Auth 使用：

```text
database
events
logger
middleware
tracing
```

Auth 已完成 go-zero 化，并成功编译通过。

### 11.3 Judge API

Judge API 当前是 go-zero 服务，但还没有完全接入 Shared。

它目前直接使用：

```text
pgxpool
nats.go
go-zero config
```

后续可以迁移到：

```text
shared/database
shared/events
shared/logger
shared/tracing
shared/middleware
```

---

## 十二、编译检查

### 12.1 编译 Shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 12.2 检查旧模块是否删除

```powershell
cd D:\Untitled-OJ\services\shared

Test-Path .\config
Test-Path .\response
```

期望：

```text
False
False
```

### 12.3 检查旧引用

```powershell
cd D:\Untitled-OJ\services

Select-String -Path .\*\**\*.go -Pattern 'ojos-shared/config'
Select-String -Path .\*\**\*.go -Pattern 'ojos-shared/response'
Select-String -Path .\*\**\*.go -Pattern 'config.Load'
Select-String -Path .\*\**\*.go -Pattern 'database.NewPostgresPool\('
Select-String -Path .\*\**\*.go -Pattern 'events.NewBus\('
Select-String -Path .\*\**\*.go -Pattern 'tracing.Init\('
```

期望没有输出。

---

## 十三、Docker 构建注意事项

依赖 Shared 的服务，Dockerfile 必须复制 Shared。

例如 Gateway / Auth：

```dockerfile
FROM golang:1.26.3

WORKDIR /app

COPY auth/go.mod auth/go.sum ./auth/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/auth
RUN go mod download

WORKDIR /app

COPY auth ./auth
COPY shared ./shared

WORKDIR /app/auth

RUN go build -o auth .

CMD ["./auth", "-f", "etc/auth.yaml"]
```

Compose build context 必须是：

```yaml
build:
  context: ../../services
  dockerfile: auth/Dockerfile
```

不能只以单个服务目录作为 context。

---

## 十四、当前完成状态

当前 Shared 状态：

```text
config 旧模块删除                     ✅
response 旧模块删除                   ✅
database 仅保留 URL 初始化             ✅
events 保留 Event + NATS Bus           ✅
logger 保留 service logger + trace      ✅
tracing 保留 InitOTLP                  ✅
middleware 保留 go-zero 适配            ✅
shared go build ./... 通过              ✅
gateway go build . 通过                 ✅
auth go build . 通过                    ✅
```

当前可以确认：

```text
Shared v0.2 已完成
```

---

## 十五、当前限制与后续计划

Shared 后续还可以继续增强：

```text
middleware 记录 HTTP status
middleware 增加 request_id
tracing 改为 BatchSpanProcessor
tracing 增加采样率配置
events 增加 JetStream 支持
database 增加事务 helper
统一 go-zero response wrapper
```

优先级建议：

```text
1. middleware 记录 status
2. 统一 response wrapper
3. events 可靠队列能力
4. tracing batch + sampling
```

---

## 十六、当前结论

Shared 当前已经从旧的“配置中心式公共模块”重构为“纯公共基础库”。

新的原则是：

```text
服务自己定义配置
shared 只接收参数并创建基础设施对象
业务逻辑不进入 shared
业务配置不进入 shared
新增业务模块不修改 shared
```

该结构可以支撑后续所有 go-zero 服务继续接入。
