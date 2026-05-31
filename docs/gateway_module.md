# OJOS Gateway 模块开发文档

## 一、模块定位

`services/gateway` 是 OJOS 平台的统一入口服务，负责对外暴露 HTTP API，并将请求转发到内部微服务。

当前 Gateway 已完成 go-zero 重构，并完成配置驱动代理能力。

当前 Gateway 监听端口：

```text
8080
```

外部访问统一入口：

```text
http://localhost:8080
```

当前已验证：

```text
GET  /health
POST /api/auth/login
GET  /api/auth/profile
GET  /api/judge/submissions/:id/cases
```

---

## 二、当前完成状态

当前 Gateway 已完成：

```text
go-zero 标准服务结构
健康检查接口
shared logger 接入
shared tracing 接入
shared database 接入
shared NATS event bus 接入
Recovery middleware
Logging / tracing middleware
配置驱动反向代理
Auth 服务代理
Judge API 服务代理
Docker Compose 部署
```

当前阶段 Gateway 可以认为是：

```text
Gateway MVP v0.2
```

相比旧版本，当前 Gateway 的核心改进是：

```text
新增模块不再必须修改 gateway Go 代码
只需要在 gateway.yaml 中添加 Proxy.Routes 配置
```

---

## 三、目录结构

当前 Gateway 目录结构：

```text
services/gateway/

├── etc/
│   └── gateway.yaml
│
├── internal/
│   ├── config/
│   │   └── config.go
│   │
│   ├── handler/
│   │   ├── healthhandler.go
│   │   └── routes.go
│   │
│   ├── logic/
│   │   └── healthlogic.go
│   │
│   ├── proxy/
│   │   └── proxy.go
│   │
│   ├── svc/
│   │   └── servicecontext.go
│   │
│   └── types/
│       └── types.go
│
├── Dockerfile
├── gateway.api
├── gateway.go
├── go.mod
└── go.sum
```

说明：

```text
handler / logic / types 由 goctl 根据 gateway.api 生成
proxy 是手写的配置驱动反向代理模块
svc 负责依赖初始化和注入
```

---

## 四、go-zero 重构说明

Gateway 已经改为 go-zero 标准 API 服务。

生成方式：

```powershell
cd D:\Untitled-OJ\services

goctl api new gateway --module ojos-gateway
```

修改 `gateway.api` 后重新生成：

```powershell
cd D:\Untitled-OJ\services\gateway

goctl api go -api gateway.api -dir .
```

当前不再使用旧的手写结构：

```text
internal/app
internal/router
手写 main + router.Register
```

而是使用 go-zero 标准结构：

```text
gateway.go
internal/config
internal/handler
internal/logic
internal/svc
internal/types
```

---

## 五、gateway.api

当前 `gateway.api` 只定义 Gateway 自身接口。

路径：

```text
services/gateway/gateway.api
```

当前内容：

```go
syntax = "v1"

info(
    title: "OJOS Gateway API"
    desc: "OJOS API Gateway"
    author: "thedyingkai"
    version: "v1"
)

type HealthResp {
    Status string `json:"status"`
}

service gateway {
    @handler health
    get /health returns (HealthResp)
}
```

说明：

```text
/health 是 Gateway 自己的接口
/api/auth/*
/api/judge/*
不写在 gateway.api 中
它们通过配置驱动代理处理
```

这样可以避免每新增一个模块都要修改 `gateway.api` 和生成代码。

---

## 六、配置文件

Gateway 配置文件：

```text
services/gateway/etc/gateway.yaml
```

当前配置：

```yaml
Name: gateway-service
Host: 0.0.0.0
Port: 8080

Database:
  Url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

Nats:
  Url: nats://ojos-nats:4222

Jaeger:
  Endpoint: ojos-jaeger:4317

Proxy:
  Routes:
    - Prefix: /api/auth
      Target: http://auth:8081
      StripPrefix: /api

    - Prefix: /api/judge
      Target: http://judge-api:8082
      StripPrefix: /api
```

字段说明：

| 字段                | 说明              |
| ----------------- | --------------- |
| `Name`            | 服务名，用于日志和链路追踪   |
| `Host`            | HTTP 监听地址       |
| `Port`            | HTTP 监听端口       |
| `Database.Url`    | PostgreSQL 连接地址 |
| `Nats.Url`        | NATS 连接地址       |
| `Jaeger.Endpoint` | OTLP gRPC 上报地址  |
| `Proxy.Routes`    | 代理路由配置          |

---

## 七、配置结构

路径：

```text
services/gateway/internal/config/config.go
```

当前结构：

```go
package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Nats     NatsConfig
    Jaeger   JaegerConfig
    Proxy    ProxyConfig
}

type DatabaseConfig struct {
    Url string
}

type NatsConfig struct {
    Url string
}

type JaegerConfig struct {
    Endpoint string
}

type ProxyConfig struct {
    Routes []ProxyRouteConfig
}

type ProxyRouteConfig struct {
    Prefix      string
    Target      string
    StripPrefix string `json:",optional"`
}
```

---

## 八、配置驱动代理设计

Gateway 当前使用配置驱动代理。

代理配置示例：

```yaml
Proxy:
  Routes:
    - Prefix: /api/auth
      Target: http://auth:8081
      StripPrefix: /api

    - Prefix: /api/judge
      Target: http://judge-api:8082
      StripPrefix: /api
```

### 8.1 Auth 转发规则

```text
请求：
POST /api/auth/login

匹配：
Prefix = /api/auth

去前缀：
StripPrefix = /api

转发为：
POST http://auth:8081/auth/login
```

### 8.2 Judge 转发规则

```text
请求：
GET /api/judge/submissions/3/cases

匹配：
Prefix = /api/judge

去前缀：
StripPrefix = /api

转发为：
GET http://judge-api:8082/judge/submissions/3/cases
```

---

## 九、代理模块

代理模块路径：

```text
services/gateway/internal/proxy/proxy.go
```

主要职责：

```text
读取 Proxy.Routes
校验 Prefix / Target
按 Prefix 长度排序
匹配请求路径
转发到对应 Target
注入 trace context
设置 X-Forwarded-* 请求头
处理上游错误
```

当前 Gateway 反向代理使用 `httputil.ReverseProxy.Rewrite`，不再使用已弃用的 `Director`。Rewrite 阶段会设置上游 URL、Host、X-Forwarded-* Header，并注入 OpenTelemetry Trace Context。

### 9.1 路由匹配规则

代理使用前缀匹配：

```text
path == prefix
或
path 以 prefix + "/" 开头
```

例如：

```text
/api/auth
/api/auth/login
/api/auth/profile
```

都会匹配：

```text
Prefix: /api/auth
```

但：

```text
/api/authentication
```

不会误匹配 `/api/auth`。

### 9.2 路由优先级

代理路由会按 `Prefix` 长度从长到短排序。

这样可以支持：

```yaml
Proxy:
  Routes:
    - Prefix: /api/judge/admin
      Target: http://judge-admin:8085
      StripPrefix: /api

    - Prefix: /api/judge
      Target: http://judge-api:8082
      StripPrefix: /api
```

更长的 `/api/judge/admin` 会优先匹配。

### 9.3 请求头

Gateway 转发请求时会设置：

```text
X-Forwarded-Host
X-Forwarded-Prefix
X-Gateway
```

同时会通过 OpenTelemetry propagator 注入 trace context，使下游服务可以继续同一条链路。

---

## 十、ServiceContext

路径：

```text
services/gateway/internal/svc/servicecontext.go
```

`ServiceContext` 负责初始化 Gateway 运行依赖：

```text
Config
Logger
DB
Tracer
Bus
Proxy
```

当前结构：

```go
type ServiceContext struct {
    Config config.Config

    Logger *zap.Logger
    DB     *pgxpool.Pool
    Tracer *sdktrace.TracerProvider
    Bus    *events.Bus

    Proxy http.HandlerFunc
}
```

初始化流程：

```text
读取 go-zero config
初始化 zap logger
初始化 OpenTelemetry tracer
连接 PostgreSQL
连接 NATS
构建配置驱动 proxy
返回 ServiceContext
```

关闭流程：

```text
关闭 NATS
关闭 PostgreSQL 连接池
关闭 TracerProvider
Sync Logger
```

---

## 十一、shared 依赖

Gateway 使用 `services/shared` 中的公共能力。

当前依赖：

```text
ojos-shared/database
ojos-shared/events
ojos-shared/logger
ojos-shared/middleware
ojos-shared/tracing
```

### 11.1 database

Gateway 使用：

```go
database.NewPostgresPoolByURL(ctx, c.Database.Url)
```

用于按 URL 初始化 PostgreSQL 连接池。

### 11.2 events

Gateway 使用：

```go
events.NewBusByURL(c.Nats.Url, c.Name)
```

用于初始化 NATS EventBus。

当前 Gateway 暂未主动发布业务事件，但已经具备事件能力。

### 11.3 logger

Gateway 使用：

```go
sharedlogger.New(c.Name)
```

创建 zap logger。

日志中会带：

```text
service = gateway-service
```

### 11.4 tracing

Gateway 使用：

```go
tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
```

初始化 OpenTelemetry OTLP exporter。

### 11.5 middleware

Gateway 使用：

```go
sharedmw.RecoveryMiddleware(...)
sharedmw.LoggingMiddleware(...)
```

用于：

```text
panic recovery
请求日志
trace_id / span_id 注入日志
请求耗时记录
```

---

## 十二、gateway.go 启动流程

路径：

```text
services/gateway/gateway.go
```

启动流程：

```text
解析 -f 参数
读取 gateway.yaml
初始化 ServiceContext
构建 NotFound Proxy Handler
创建 go-zero rest server
注册 Recovery Middleware
注册 Logging Middleware
注册 goctl 生成的 Handler
启动 HTTP Server
```

关键设计：

```text
go-zero 明确注册 /health
未匹配到的请求进入 WithNotFoundHandler
WithNotFoundHandler 中执行配置驱动代理
```

也就是说：

```text
/health
    -> go-zero handler

/api/auth/*
/api/judge/*
    -> NotFoundHandler
    -> config proxy
```

---

## 十三、健康检查接口

### 请求

```http
GET /health
```

### 响应

```json
{
  "status": "ok"
}
```

当前已验证：

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/health"
```

返回：

```text
status
------
ok
```

---

## 十四、Auth 代理接口

Gateway 当前代理：

```text
/api/auth/*
```

到：

```text
http://auth:8081
```

### 14.1 登录

请求：

```http
POST /api/auth/login
Content-Type: application/json
```

请求体：

```json
{
  "username": "admin",
  "password": "123456"
}
```

实际转发到：

```http
POST http://auth:8081/auth/login
```

返回示例：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "token": "...",
    "user_id": 1,
    "username": "admin",
    "roles": ["user"]
  }
}
```

### 14.2 Profile

请求：

```http
GET /api/auth/profile
Authorization: Bearer <token>
```

实际转发到：

```http
GET http://auth:8081/auth/profile
```

返回示例：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "user_id": 1,
    "username": "admin",
    "roles": ["user"]
  }
}
```

---

## 十五、Judge 代理接口

Gateway 当前代理：

```text
/api/judge/*
```

到：

```text
http://judge-api:8082
```

### 15.1 查询测试点详情

请求：

```http
GET /api/judge/submissions/3/cases
```

实际转发到：

```http
GET http://judge-api:8082/judge/submissions/3/cases
```

返回示例：

```json
{
  "cases": [
    {
      "id": 1,
      "submission_id": 3,
      "test_case_id": 1,
      "status": "ACCEPTED",
      "time_ms": 4,
      "memory_kb": 0,
      "message": ""
    }
  ]
}
```

---

## 十六、Dockerfile

路径：

```text
services/gateway/Dockerfile
```

当前内容：

```dockerfile
FROM golang:1.26.3

WORKDIR /app

COPY gateway/go.mod gateway/go.sum ./gateway/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/gateway
RUN go mod download

WORKDIR /app

COPY gateway ./gateway
COPY shared ./shared

WORKDIR /app/gateway

RUN go build -o gateway .

CMD ["./gateway", "-f", "etc/gateway.yaml"]
```

说明：

```text
gateway 依赖 shared，所以 build context 必须是 ../../services
Dockerfile 同时复制 gateway 和 shared
```

---

## 十七、Docker Compose

Gateway Compose 配置：

```yaml
gateway:
  build:
    context: ../../services
    dockerfile: gateway/Dockerfile
  container_name: ojos-gateway
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

说明：

```text
gateway 需要 postgres / nats / jaeger
gateway 对外暴露 8080
内部通过 Docker Compose service name 访问 auth / judge-api
```

当前代理依赖以下服务名：

```text
auth
judge-api
```

如果 Compose 中服务名改变，必须同步修改：

```text
services/gateway/etc/gateway.yaml
```

---

## 十八、编译与启动

### 18.1 编译 shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 18.2 编译 gateway

```powershell
cd D:\Untitled-OJ\services\gateway

go mod tidy
go build .
```

### 18.3 Docker 启动

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build gateway
```

### 18.4 查看日志

```powershell
docker logs ojos-gateway
```

预期：

```text
Starting server at 0.0.0.0:8080...
```

---

## 十九、当前验收结果

当前已验证：

### 19.1 Gateway 启动

```powershell
docker logs ojos-gateway
```

结果：

```text
Starting server at 0.0.0.0:8080...
```

### 19.2 Health

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/health"
```

结果：

```text
status
------
ok
```

### 19.3 Auth Login 代理

```powershell
$body = @{
  username = "admin"
  password = "123456"
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/login" `
  -ContentType "application/json" `
  -Body $body
```

结果：

```text
code = 0
msg = success
data.token 存在
```

### 19.4 Auth Profile 代理

```powershell
$token = $res.data.token

Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

结果：

```text
code = 0
msg = success
data.user_id = 1
data.username = admin
```

### 19.5 Judge API 代理

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/3/cases"
```

结果：

```text
cases:
  submission_id = 3
  test_case_id = 1
  status = ACCEPTED
```

---

## 二十、当前设计优点

当前 Gateway 相比旧版本的主要改进：

```text
使用 go-zero 标准结构
使用 goctl 生成 handler / logic / types
shared 能力通过 ServiceContext 注入
代理规则配置化
新增模块无需修改 gateway Go 代码
支持 trace context 传播
支持统一 recovery / logging middleware
```

新增服务时，只需修改：

```yaml
Proxy:
  Routes:
    - Prefix: /api/new-service
      Target: http://new-service:808x
      StripPrefix: /api
```

不需要新增：

```text
router.Register(...)
server.AddRoute(...)
handler.NewXXX(...)
```

---

## 二十一、当前限制

当前 Gateway 仍然不是最终完整网关。

### 21.1 没有鉴权前置

Gateway 当前只是转发请求，没有统一鉴权。

例如：

```text
/api/judge/*
```

当前没有在 Gateway 层校验 JWT。

后续可以选择：

```text
方案一：每个服务自己鉴权
方案二：Gateway 统一鉴权后透传用户信息
方案三：Gateway 做粗粒度鉴权，服务做细粒度鉴权
```

推荐后续采用：

```text
Gateway 校验 JWT
向下游透传：
X-User-Id
X-Username
X-Roles
```

---

### 21.2 没有限流

当前没有全局限流和接口级限流。

后续需要补：

```text
IP 限流
用户限流
提交频率限制
登录频率限制
```

---

### 21.3 没有熔断和重试策略

当前反向代理只做简单转发。

后续可以补：

```text
上游超时
失败重试
服务熔断
健康检查
```

---

### 21.4 没有统一响应格式

当前 Gateway 自己的 `/health` 返回：

```json
{
  "status": "ok"
}
```

Auth 返回：

```json
{
  "code": 0,
  "msg": "success",
  "data": {}
}
```

Judge API 返回 go-zero 默认结构。

后续需要决定是否统一响应格式。

---

### 21.5 没有服务发现

当前上游地址写死在：

```text
gateway.yaml
```

后续如果服务规模变大，可以接入：

```text
etcd
consul
kubernetes service
```

---

### 21.6 代理配置不是热更新

当前 `gateway.yaml` 只在启动时加载。

修改代理配置后需要重启 Gateway。

---

## 二十二、后续计划

Gateway 后续建议开发顺序：

```text
1. 统一 JWT 鉴权中间件
2. 向下游透传用户信息 Header
3. 配置代理超时时间
4. 配置代理限流
5. 配置代理是否需要鉴权
6. Gateway 统一错误格式
7. Gateway 支持配置热更新
8. 接入服务发现
```

建议下一阶段先做：

```text
Gateway JWT 鉴权 + 用户信息透传
```

因为后续 Judge、Problem、Contest 都需要知道当前用户是谁。

---

## 二十三、当前结论

当前 Gateway 已经完成当前阶段必要能力：

```text
go-zero 重构完成
/health 正常
配置驱动代理完成
Auth 代理正常
Judge 代理正常
shared 基础设施接入完成
Docker Compose 部署正常
```

当前 Gateway 可以作为 OJOS 各服务的统一 HTTP 入口继续使用。

下一阶段重点不是继续重写 Gateway，而是补充：

```text
鉴权
限流
超时
服务治理
```
