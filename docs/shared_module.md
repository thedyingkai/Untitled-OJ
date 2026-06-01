# OJOS Shared 模块开发文档

## 一、模块定位

`services/shared` 是 OJOS Go 微服务体系中的公共基础库。

它不是一个独立运行的服务，不监听端口，不提供 HTTP API，也不应该被 `goctl api new` 生成。它的定位更接近：

```text id="najggq"
OJOS Go Service SDK
```

也就是供各个 Go 微服务复用的基础能力集合。

当前已经接入 Shared 的服务包括：

```text id="n5s0jd"
services/gateway
services/auth
services/judge-api
```

后续新增 Go 微服务也应优先复用 Shared，例如：

```text id="l6n81i"
services/problem-api
services/contest-api
services/scoreboard-api
services/permission-api
services/module-registry
services/launcher
```

Shared 的目标是让各业务服务不重复实现：

```text id="1b6ryh"
数据库连接池
结构化日志
链路追踪
HTTP 中间件
JWT 生成与解析
可信用户上下文解析
资源级权限检查
```

同时，Shared 必须严格避免变成业务逻辑垃圾桶。

Shared 不应该负责：

```text id="yngzsu"
题目管理
比赛管理
提交管理
判题逻辑
榜单计算
模块安装
业务配置决策
HTTP 路由注册
服务生命周期编排
```

Shared 的边界应保持稳定：

```text id="q0frsf"
只提供基础设施能力
不提供具体业务能力
只接收参数
不加载具体服务配置
只暴露通用函数
不保存业务状态
```

---

## 二、当前完成状态

当前 Shared 已经完成 go-zero 适配，并完成旧兼容层清理。

当前状态可以记为：

```text id="z9bo5j"
Shared v0.3+
```

当前已完成能力：

```text id="o89otm"
独立 Go module
PostgreSQL URL 初始化
Zap logger 初始化
trace_id / span_id 日志注入
OpenTelemetry OTLP 初始化
go-zero Recovery Middleware
go-zero Logging Middleware
JWT 生成与解析
可信用户上下文 Header 解析
完整资源级 Permission Core
角色绑定
直接授权 / 拒绝
资源继承关系维护
权限点注册
资源类型注册
shared 自身 go build ./... 通过
gateway 接入通过
auth 接入通过
judge-api 接入通过
```

当前已经删除旧模块：

```text id="yj2c3m"
shared/config
shared/response
shared/events
shared/events/event.go
shared/events/nats.go
```

当前 Shared 不再包含：

```text id="bpam0h"
NATS EventBus
统一 Event 结构
业务事件发布器
Viper 配置加载器
旧 response 包装器
```

这些删除是有意为之，不是遗漏。

原因如下：

```text id="2uge7h"
1. 配置结构应该由各服务自己定义
2. 统一 response 需要结合 go-zero 错误处理重新设计
3. Judge 任务队列已迁移到 Redis Streams
4. 当前系统不再使用 NATS
5. Shared 不应该提前封装一个未稳定的事件总线抽象
```

---

## 三、当前目录结构

当前 Shared 推荐目录结构如下：

```text id="u6wk94"
services/shared/

├── database/
│   └── postgres.go
│
├── logger/
│   └── logger.go
│
├── middleware/
│   ├── gozero.go
│   ├── logging.go
│   └── recovery.go
│
├── security/
│   ├── authctx/
│   │   └── authctx.go
│   │
│   ├── jwt/
│   │   └── jwt.go
│   │
│   └── permission/
│       └── permission.go
│
├── tracing/
│   └── tracing.go
│
├── go.mod
└── go.sum
```

当前不应存在：

```text id="3ogw06"
services/shared/config
services/shared/response
services/shared/events
```

如果后续需要事件系统，也不应该直接恢复旧的 `events/nats.go`，而应该重新设计为：

```text id="n96ldm"
Redis Streams task queue helper
或者
module event adapter
或者
domain event interface
```

并且要先明确：

```text id="5wvxrm"
这是可靠任务队列
还是普通广播事件
是否需要持久化
是否需要 ACK
是否需要重试
是否需要 consumer group
```

当前阶段不建议在 Shared 中重新加入事件总线。

---

## 四、go.mod

Shared 是独立 Go module。

模块名：

```go id="lhzbe2"
module ojos-shared
```

其他服务使用 Shared 时，在自己的 `go.mod` 中加入：

```go id="nnq21m"
require ojos-shared v0.0.0

replace ojos-shared => ../shared
```

例如：

```go id="z3ilq4"
module ojos-auth

go 1.26

require ojos-shared v0.0.0

replace ojos-shared => ../shared
```

当前 `replace ../shared` 是 monorepo 本地开发的正常做法。GoLand 可能会提示：

```text id="abdlhd"
提交本地路径可能无法移植
```

当前可以接受。原因是：

```text id="7x46nh"
1. auth / gateway / judge-api 与 shared 位于同一个 monorepo
2. Docker build context 已按 monorepo 结构组织
3. 当前没有把 shared 发布成独立远程 module
4. 使用 replace 可以保证本地开发和容器构建一致
```

后续如果 OJOS 拆分为多个仓库，可以再考虑：

```text id="vnj08a"
发布 ojos-shared 到独立 Git module
或使用 go.work 做本地开发
或引入统一 workspace 构建脚本
```

当前 `.gitignore` 中忽略：

```text id="1xi5jz"
go.work
go.work.sum
```

是合理的。`go.work` 可以作为个人本地开发配置，不强制提交。

---

## 五、Shared 的设计原则

Shared 应遵守以下原则。

### 5.1 不保存业务配置

Shared 不应该提供：

```go id="b57dal"
config.Load()
```

也不应该定义统一的全局配置结构，例如：

```go id="nwpu5n"
type Config struct {
    Service  ServiceConfig
    Database DatabaseConfig
    Nats     NatsConfig
    Jaeger   JaegerConfig
}
```

原因是不同服务的配置并不完全一致。

例如：

```text id="sua8n0"
auth 需要 Jwt
gateway 需要 Proxy.Routes
judge-api 需要 Redis
problem-api 未来需要 Storage
contest-api 未来需要 RuleConfig
launcher 未来需要 ModuleRegistry
```

所以当前正确方式是：

```text id="s2oqmu"
每个服务自己定义 internal/config/config.go
每个服务自己定义 etc/*.yaml
Shared 只提供基础设施初始化函数
服务把具体 URL / Secret / Endpoint 传给 Shared
```

例如：

```go id="0tbf97"
db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
```

而不是：

```go id="xknsq8"
db, err := database.NewPostgresPool(ctx, c)
```

---

### 5.2 不提供业务响应格式

旧的 `shared/response` 已删除。

当前原因是：

```text id="opqlpg"
go-zero 有自己的 handler / logic / error 处理方式
统一响应格式需要结合 go-zero error handler 重新设计
不同服务需要区分业务错误、权限错误、认证错误、系统错误
当前错误响应尚未统一
```

后续如果要恢复统一响应能力，应单独设计：

```text id="5kfc1x"
shared/errors
shared/httperror
shared/response
```

并明确错误码规范，例如：

```text id="72mijt"
40101 missing authorization header
40102 invalid token
40301 forbidden
40001 invalid request
50001 internal server error
50201 bad gateway
```

当前下一阶段计划中已经包含：

```text id="6qyhn8"
统一错误响应，尤其是 forbidden -> JSON
```

所以当前不要随便恢复旧 `response` 包。

---

### 5.3 不绑定消息队列实现

旧的 Shared 包含：

```text id="lcjs37"
events/event.go
events/nats.go
```

现在已删除。

原因是当前 Judge 任务链路已经从：

```text id="zqrjw2"
NATS Core Pub/Sub
```

迁移到：

```text id="5wmmwv"
Redis Streams Consumer Group
```

而且可靠任务队列和普通事件广播不是同一个抽象。

可靠任务队列需要：

```text id="ld0m6r"
持久化
ACK
Pending list
Consumer group
失败重试
积压查看
```

普通事件广播更关注：

```text id="lp3hn4"
低延迟
多订阅者
事件通知
不一定可靠
```

如果 Shared 提前抽象一个 `EventBus`，很容易把两者混成一个错误模型。

当前更合理的做法是：

```text id="a66g1t"
judge-api 自己使用 Redis XADD
judge-worker 自己使用 XREADGROUP
未来再根据多个模块重复需求抽象 shared/queue
```

也就是说，先不要为了“看起来统一”而过早抽象。

---

### 5.4 不写业务 SQL

Shared 只能提供通用 SQL 辅助能力。

当前例外是：

```text id="5myc25"
security/permission
```

它确实会访问权限核心表。

这是合理的，因为 Permission Core 是跨业务模块的基础能力，不属于某个单独业务服务。

但 Shared 不应该包含：

```text id="3v9vy6"
CreateProblem
CreateSubmission
CreateContest
UpdateScoreboard
```

这些应分别属于：

```text id="n0cuzq"
problem-api
judge-api
contest-api
scoreboard-api
```

---

### 5.5 不持有服务生命周期

Shared 不应该启动 HTTP server。

Shared 不应该监听端口。

Shared 不应该自己调用：

```go id="1m0iiy"
server.Start()
```

Shared 只应该返回基础设施对象，例如：

```go id="3xyx2g"
*zap.Logger
*pgxpool.Pool
*sdktrace.TracerProvider
rest.Middleware
```

由业务服务的 `ServiceContext` 或主程序负责持有和关闭。

---

## 六、database 模块

路径：

```text id="ci6vmy"
services/shared/database
```

推荐文件：

```text id="2bvk66"
services/shared/database/postgres.go
```

### 6.1 模块职责

`database` 模块负责统一创建 PostgreSQL 连接池。

当前使用：

```text id="r5l7y3"
github.com/jackc/pgx/v5/pgxpool
```

当前推荐函数：

```go id="qckd6m"
func NewPostgresPoolByURL(
    ctx context.Context,
    databaseURL string,
) (*pgxpool.Pool, error)
```

功能：

```text id="5yby1r"
接收 PostgreSQL URL
解析 pgxpool 配置
创建连接池
Ping 数据库
连接失败时释放连接池
返回 *pgxpool.Pool
```

使用方式：

```go id="7dw5ic"
db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
if err != nil {
    log.Fatalf("connect postgres failed: %v", err)
}
```

---

### 6.2 为什么只接收 URL

之前的设计可能是：

```go id="de7kix"
database.NewPostgresPool(ctx, cfg)
```

这会导致 Shared 依赖某个配置结构。

现在改为：

```go id="qx78yu"
database.NewPostgresPoolByURL(ctx, url)
```

好处是：

```text id="9hoc1l"
Shared 不关心服务配置结构
每个服务可以有不同配置字段
减少 shared/config 的必要性
降低服务间耦合
更适合 go-zero 每个服务独立 config 的模式
```

---

### 6.3 连接 URL 约定

在 Docker 容器内部，PostgreSQL URL 应使用容器服务名：

```text id="8qm0cs"
postgres://postgres:password@postgres:5432/ojos?sslmode=disable
```

在宿主机执行 migrate 时，应使用映射端口：

```text id="bchh54"
postgres://postgres:password@localhost:5433/ojos?sslmode=disable
```

不要在容器内写：

```text id="hpdzhu"
localhost:5433
```

因为容器内的 `localhost` 指向当前容器本身。

---

### 6.4 使用位置

当前使用 database 模块的服务包括：

```text id="x9n9nv"
gateway
auth
judge-api
```

示例：

```go id="7kgib3"
ctx := context.Background()

db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
if err != nil {
    log.Fatalf("connect postgres failed: %v", err)
}
```

服务关闭时应调用：

```go id="3ngn7d"
db.Close()
```

如果服务目前没有实现显式关闭流程，也至少要保证 `db` 由 `ServiceContext` 持有，后续统一补充关闭逻辑。

---

## 七、logger 模块

路径：

```text id="bk4fhi"
services/shared/logger
```

推荐文件：

```text id="2wxvoo"
services/shared/logger/logger.go
```

### 7.1 模块职责

`logger` 模块负责创建统一结构化日志器。

当前使用：

```text id="ntyeff"
go.uber.org/zap
```

推荐能力：

```text id="7nr4ge"
创建服务级 logger
日志中附带 service 字段
支持从 context 注入 trace_id / span_id
支持 Sync
```

推荐函数：

```go id="2luhf0"
func New(serviceName string) (*zap.Logger, error)
```

或者当前已有类似函数也可以保持。

---

### 7.2 日志字段约定

所有服务日志应尽量带上：

```text id="w4fpdv"
service
trace_id
span_id
method
path
status
duration
error
user_id
submission_id
problem_id
```

不同模块可以追加自己的字段。

例如 Gateway HTTP 请求日志：

```json id="2mup1u"
{
  "level": "info",
  "service": "gateway-service",
  "trace_id": "56af8e5d99c1e0f39afcc2f144f63101",
  "span_id": "aa96fb71d55bb95f",
  "method": "GET",
  "path": "/health",
  "status": 200,
  "duration": 0.000309788
}
```

Judge API 创建提交日志后续可以包含：

```json id="elv3kg"
{
  "service": "judge-api-service",
  "user_id": 2,
  "problem_id": 1,
  "submission_id": 16,
  "language": "cpp17"
}
```

---

### 7.3 trace 注入

推荐函数：

```go id="dpj2ji"
func WithTrace(ctx context.Context, log *zap.Logger) *zap.Logger
```

功能：

```text id="v6oyfq"
从 context 中读取当前 span
提取 trace_id
提取 span_id
返回带 trace 字段的 logger
```

注意：

```text id="xym7u1"
logger.WithTrace 不负责创建 span
logger.WithTrace 不负责结束 span
logger.WithTrace 不负责上报 Jaeger
```

创建和上报 span 由 tracing 和 middleware 负责。

---

### 7.4 日志等级建议

推荐等级：

```text id="hwgx7r"
debug   开发调试信息，例如定期扫描无任务
info    正常业务事件，例如服务启动、提交创建、任务完成
warn    可恢复异常，例如重复任务、无效消息、跳过处理
error   需要关注的错误，例如数据库失败、Redis 失败、判题失败
fatal   服务无法启动，例如配置错误、数据库连接失败
```

当前 Judge Worker 中：

```text id="6s2sff"
no pending submissions found
```

建议不要用 `info` 高频打印。可以：

```text id="a2a5nh"
降为 debug
或者不打印
只在发现 pending 时打印 info
```

---

## 八、tracing 模块

路径：

```text id="qqe7fi"
services/shared/tracing
```

推荐文件：

```text id="48n2og"
services/shared/tracing/tracing.go
```

### 8.1 模块职责

`tracing` 模块负责初始化 OpenTelemetry。

当前使用：

```text id="u2t3jm"
OpenTelemetry
OTLP gRPC
Jaeger
```

推荐函数：

```go id="2c8m16"
func InitOTLP(
    ctx context.Context,
    serviceName string,
    endpoint string,
) (*sdktrace.TracerProvider, error)
```

功能：

```text id="9gl3uf"
创建 OTLP exporter
创建 resource
设置 service.name
创建 TracerProvider
设置全局 TracerProvider
设置全局 TextMapPropagator
返回 TracerProvider
```

---

### 8.2 配置字段

各服务配置中一般包含：

```yaml id="xfsr6e"
Jaeger:
  Endpoint: ojos-jaeger:4317
```

这里虽然字段叫 `Jaeger.Endpoint`，实际上走的是：

```text id="xk51t3"
OTLP gRPC endpoint
```

后续可以考虑改名为：

```yaml id="g8ctwb"
Tracing:
  Endpoint: ojos-jaeger:4317
```

但当前为了减少重构，保留 `Jaeger.Endpoint` 也可以。

---

### 8.3 使用方式

在服务初始化时：

```go id="p1nf8e"
tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
if err != nil {
    log.Fatalf("init tracing failed: %v", err)
}
```

服务关闭时：

```go id="w1a15z"
_ = tp.Shutdown(context.Background())
```

---

### 8.4 Trace Context 传播

Gateway 作为入口，应读取外部请求并继续传播 trace context。

Gateway 反向代理到下游服务时，应注入：

```text id="e5hq61"
traceparent
tracestate
```

在 Go 中通常通过：

```go id="38qs4y"
otel.GetTextMapPropagator().Inject(
    req.Context(),
    propagation.HeaderCarrier(req.Header),
)
```

下游服务通过 HTTP middleware 继续接收。

当前 Redis Streams 队列尚未完整传播 trace context。

后续可以考虑在 Redis Stream 消息中加入：

```text id="ysnzm7"
traceparent
tracestate
```

这样 judge-api 到 judge-worker 的异步链路也能串起来。

---

### 8.5 关于 Zipkin 警告

GoLand 可能提示：

```text id="xrww5t"
go.opentelemetry.io/otel/exporters/zipkin 已弃用
```

当前原因通常不是业务代码直接使用 Zipkin，而是：

```text id="rzzien"
go-zero/core/trace
```

仍间接 import 相关包。

当前处理原则：

```text id="68v35k"
不要手动删除 go.sum 中 go-zero 需要的 zipkin 校验项
保证 go build 通过
后续通过升级 go-zero 或替换 trace 初始化方式解决
```

如果出现：

```text id="c37lip"
missing go.sum entry for module providing package go.opentelemetry.io/otel/exporters/zipkin
```

可以执行：

```powershell id="5v034r"
go get github.com/zeromicro/go-zero/core/trace@v1.10.2
go mod tidy
```

当前不应把 zipkin 警告和 NATS 清理混为一谈。

---

## 九、middleware 模块

路径：

```text id="trt0gm"
services/shared/middleware
```

当前 middleware 模块用于适配 go-zero。

推荐文件：

```text id="lt6spt"
services/shared/middleware/gozero.go
services/shared/middleware/logging.go
services/shared/middleware/recovery.go
```

---

### 9.1 Recovery Middleware

职责：

```text id="byvz4u"
捕获 panic
记录 panic 日志
防止服务进程崩溃
返回 500 响应
```

推荐提供：

```go id="1mzv6i"
func RecoveryMiddleware(log *zap.Logger) rest.Middleware
```

使用方式：

```go id="ufptai"
server.Use(sharedmw.RecoveryMiddleware(log))
```

Recovery 应记录：

```text id="fb6pei"
panic value
method
path
trace_id
span_id
stack trace
```

当前如果还没有 stack trace，可以后续补充。

---

### 9.2 Logging Middleware

职责：

```text id="ydjye8"
记录 HTTP 请求日志
记录 method
记录 path
记录 status
记录 duration
注入 trace 信息
```

推荐提供：

```go id="rotwxj"
func LoggingMiddleware(log *zap.Logger) rest.Middleware
```

使用方式：

```go id="j9ckac"
server.Use(sharedmw.LoggingMiddleware(log))
```

日志建议包含：

```text id="tq3pdm"
method
path
status
duration
remote_addr
user_agent
trace_id
span_id
```

---

### 9.3 go-zero 适配注意事项

go-zero 的 middleware 形式是：

```go id="cdp1eq"
type Middleware func(next http.HandlerFunc) http.HandlerFunc
```

所以 Shared 中间件要返回：

```go id="w3b9v6"
rest.Middleware
```

而不是旧式：

```go id="slbuxv"
func(next http.Handler) http.Handler
```

之前曾经出现过 `middleware/gozero.go` 语法问题，原因通常是：

```text id="jyg4wv"
package 声明前有多余文本
import 写在声明之后
复制代码时混入说明文字
```

正确 Go 文件必须以：

```go id="asq9xz"
package middleware

import (
    ...
)
```

开头。

---

## 十、security/jwt 模块

路径：

```text id="ugwpfo"
services/shared/security/jwt
```

### 10.1 模块职责

`security/jwt` 负责 JWT 的生成和解析。

当前 Auth 使用它签发 token，Gateway 使用它解析 token。

该模块应支持：

```text id="k5ldxb"
user_id
username
roles
issuer
subject
issued_at
expires_at
```

推荐 Claims 包含：

```text id="de8m3v"
user_id
username
roles
iss
sub
iat
exp
```

---

### 10.2 生成 Token

推荐函数形式：

```go id="esj2j7"
func Generate(
    secret string,
    expireHours int,
    userID int64,
    username string,
    roles []string,
) (string, error)
```

注意参数顺序要统一，避免之前出现过的错误：

```text id="camita"
cannot use s.jwtExpireHours as int64
cannot use userID as string
cannot use username as []string
cannot use roles as int
```

这类错误说明调用方和函数签名不一致。

推荐固定顺序：

```text id="xkxdvq"
secret
expireHours
userID
username
roles
```

不要来回调整。

---

### 10.3 解析 Token

推荐函数形式：

```go id="rhdcu0"
func Parse(
    secret string,
    tokenString string,
) (*Claims, error)
```

解析后返回：

```go id="jaaqm7"
type Claims struct {
    UserID   int64
    Username string
    Roles    []string
    jwt.RegisteredClaims
}
```

Gateway 解析成功后，应生成可信 Header：

```text id="fpv3fv"
X-Auth-Verified: true
X-User-Id: <id>
X-Username: <username>
X-Roles: <role1,role2>
```

---

### 10.4 安全注意事项

JWT Secret 当前开发环境可以写：

```yaml id="fdigpo"
Jwt:
  Secret: ojos-dev-secret-change-me
```

但生产必须替换为强随机密钥。

不应将生产 Secret 提交到 Git。

`.gitignore` 中应忽略：

```text id="2fb6bd"
.env
.env.*
```

但保留：

```text id="c81m90"
.env.example
```

后续可以将 Secret 改为：

```text id="r4uwtm"
环境变量注入
Docker secret
Kubernetes secret
Vault
```

---

## 十一、security/authctx 模块

路径：

```text id="n2g041"
services/shared/security/authctx
```

### 11.1 模块职责

`authctx` 负责在下游服务中表示可信用户上下文。

它不负责解析 JWT。

JWT 只应由 Gateway 或 Auth 自身解析。

下游服务只读取 Gateway 注入的可信 Header，并转换成 context 中的用户对象。

---

### 11.2 用户上下文结构

推荐结构：

```go id="z1h624"
type UserContext struct {
    UserID   int64
    Username string
    Roles    []string
}
```

推荐函数：

```go id="910b55"
func WithContext(ctx context.Context, user *UserContext) context.Context
func FromContext(ctx context.Context) (*UserContext, bool)
```

judge-api 当前使用：

```go id="cx67fu"
user, ok := authctx.FromContext(l.ctx)
```

然后用：

```go id="3phx6c"
user.UserID
```

写入 submissions。

---

### 11.3 Header 信任边界

下游服务不应该信任客户端直接传来的：

```text id="7u3fuv"
X-Auth-Verified
X-User-Id
X-Username
X-Roles
```

这些 Header 必须由 Gateway 注入。

Gateway 在转发前应先清理客户端伪造值，再重新设置。

当前边界是：

```text id="47jp5g"
客户端 -> Gateway: Authorization Bearer token
Gateway -> 下游服务: X-Auth-Verified / X-User-Id / X-Username / X-Roles
```

下游服务只接受 Gateway 后面的 Header。

---

### 11.4 UserContextMiddleware

judge-api 中有自己的：

```text id="sn1q7m"
internal/middleware/usercontextmiddleware.go
```

它负责从 Header 解析用户上下文并写入 context。

后续可以考虑把这个中间件抽象到 Shared，例如：

```text id="cofd43"
shared/security/authctx/middleware.go
```

但当前不强制。原因是：

```text id="s6rm0z"
不同服务可能对匿名访问、optional auth、required auth 有不同策略
过早抽象可能限制业务服务
```

当前保持每个服务按需接入即可。

---

## 十二、security/permission 模块

路径：

```text id="zy3ojq"
services/shared/security/permission
```

Permission Core 是 Shared 中最重要的模块之一。

它提供完整资源级权限判断。

---

### 12.1 核心目标

Permission Core 解决：

```text id="hzqw90"
谁可以在什么资源范围内执行什么操作
```

统一抽象为：

```text id="i3cqxr"
Can(principal, permission, scope)
```

例如：

```text id="pxdlgy"
Can(user:1, "judge.submit", system:0)
Can(user:2, "problem.edit", problem:7)
Can(user:3, "contest.manage", contest:5)
Can(user:4, "balloon.manage", contest:5)
Can(user:5, "module.install", system:0)
```

---

### 12.2 核心类型

```go id="9syzpp"
type Principal struct {
    Type string
    ID   int64
}

type Scope struct {
    Type string
    ID   int64
}
```

当前主要使用：

```text id="jmq44w"
PrincipalUser = "user"
```

但模型预留：

```text id="g7x8jd"
team
group
service
```

作用域支持：

```text id="701lkq"
system:0
problem:7
contest:3
submission:100
module:0
```

---

### 12.3 核心函数

当前提供：

```text id="pbnk76"
HasUserPermission
RequireUserPermission
HasPermission
BindRole
AssignPermission
AddResourceEdge
RegisterResourceType
RegisterPermission
GrantRolePermission
```

典型使用：

```go id="3ewnmj"
if err := permission.RequireUserPermission(
    ctx,
    db,
    user.UserID,
    "judge.submit",
    permission.SystemScope(),
); err != nil {
    return nil, err
}
```

---

### 12.4 判断顺序

Permission Core 的判断顺序是：

```text id="12pzf2"
1. super_admin 直接允许
2. 收集当前 scope、父级 scope、type:0、system:0
3. 检查 permission_assignments.deny
4. 检查 permission_assignments.allow
5. 检查全局 user_roles
6. 检查资源级 role_bindings
7. 默认拒绝
```

其中：

```text id="tuv2xw"
deny 优先于普通 allow 和角色权限
super_admin 高于 deny
role_permissions 不带 scope
role_bindings 带 scope
```

---

### 12.5 数据库依赖

Permission Core 依赖以下表：

```text id="8h8auv"
users
roles
user_roles
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

这些表由：

```text id="8fnra2"
000003_permission_core
```

migration 创建。

---

### 12.6 当前真实接入

当前真实接入点：

```text id="4fsq96"
judge-api POST /judge/submissions
```

检查权限：

```text id="ldkqke"
judge.submit @ system:0
```

当前验证结果：

```text id="n0rtyi"
普通 user 可以提交
permission_assignments.deny 可以禁止普通 user 提交
删除 deny 后恢复
super_admin 不受 deny 影响
```

---

## 十三、当前 Shared 不再包含 events

旧 Shared 文档中存在：

```text id="1edwh2"
events/event.go
events/nats.go
```

当前已经删除。

当前不再支持：

```go id="r5cey9"
events.New(...)
events.NewBusByURL(...)
bus.Publish(...)
```

旧代码中如果还有：

```go id="6qbkvh"
ojos-shared/events
events.NewBusByURL
NewBus
EventBus
```

都应该删除。

全项目检查命令：

```powershell id="ysexcl"
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222|ojos-shared/events|NewBusByURL"
```

预期：

```text id="tdshje"
无输出
```

如果 `Cargo.lock` 中有：

```text id="nrbwyg"
event-listener
```

不用处理，它是 Redis / async 生态依赖，不是 NATS。

---

## 十四、当前 Shared 不再包含 response

旧 Shared 文档中存在：

```text id="v0dakc"
response/response.go
```

当前已经删除。

删除原因：

```text id="liz52l"
旧 response 与 go-zero 错误处理方式不完全匹配
当前错误响应尚未统一
权限错误还需要从 forbidden 文本改为统一 JSON
不同服务需要统一错误码体系
```

后续建议单独设计：

```text id="tzqaa6"
shared/errors
shared/httpresp
shared/errorx
```

统一支持：

```text id="saxumk"
错误码
HTTP status
业务 message
trace_id
内部错误包装
权限错误
认证错误
参数错误
系统错误
```

示例目标：

```json id="1yf0u4"
{
  "code": 40301,
  "msg": "forbidden",
  "trace_id": "..."
}
```

当前不要恢复旧 `response`。

---

## 十五、当前 Shared 不再包含 config

旧 Shared 文档中存在：

```text id="l72uwm"
config/config.go
config/load.go
```

当前已经删除。

删除原因：

```text id="4jyof3"
go-zero 服务有自己的 config 结构
不同服务配置不同
shared/config 会迫使所有服务共享同一个配置模型
后续模块越多，统一 config 越难维护
```

当前正确方式：

```text id="6jocyl"
auth 定义 auth/internal/config/config.go
gateway 定义 gateway/internal/config/config.go
judge-api 定义 judge-api/internal/config/config.go
problem-api 未来定义自己的 config.go
contest-api 未来定义自己的 config.go
```

Shared 只接受具体参数。

例如：

```go id="v3ef79"
database.NewPostgresPoolByURL(ctx, c.Database.Url)
tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
logger.New(c.Name)
```

---

## 十六、各服务接入 Shared 的方式

### 16.1 Auth

Auth 使用 Shared：

```text id="lu8rah"
database
logger
middleware
tracing
security/jwt
```

Auth 不再使用：

```text id="4qv9h5"
events
NATS
shared/config
shared/response
```

Auth 主要流程：

```text id="p15r24"
初始化 logger
初始化 tracing
初始化 db
初始化 repository
初始化 auth service
初始化 auth middleware
```

---

### 16.2 Gateway

Gateway 使用 Shared：

```text id="odnzz1"
database
logger
middleware
tracing
security/jwt
```

Gateway 不再使用：

```text id="lbhz9u"
events
NATS
```

Gateway 负责 JWT 验证时使用：

```text id="qbm6ay"
security/jwt
```

Gateway 透传用户上下文时遵循 `authctx` 数据模型。

---

### 16.3 Judge API

Judge API 使用 Shared：

```text id="ujsq8m"
security/authctx
security/permission
```

后续建议也统一接入：

```text id="7yc3qa"
logger
tracing
middleware
database
```

当前 Judge API 创建提交时调用：

```go id="8vkext"
permission.RequireUserPermission(
    ctx,
    db,
    user.UserID,
    "judge.submit",
    permission.SystemScope(),
)
```

---

## 十七、编译与验证

### 17.1 Shared 编译

```powershell id="xfq3cq"
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

预期：

```text id="pdlx2q"
无错误
```

---

### 17.2 检查 NATS 是否清理干净

```powershell id="o3sz4t"
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222|ojos-shared/events|NewBusByURL"
```

预期：

```text id="wteccd"
无输出
```

注意：

```text id="6auuj3"
event-listener 不是 NATS
zipkin 不是 NATS
```

不要误删。

---

### 17.3 检查 permission.go 是否进入版本管理

```powershell id="rujkvw"
cd D:\Untitled-OJ

git ls-files services/shared/security/permission/permission.go
```

预期输出：

```text id="r3ilvs"
services/shared/security/permission/permission.go
```

如果没有输出，但文件存在：

```powershell id="s5jp8b"
git add services/shared/security/permission/permission.go
```

---

### 17.4 检查 shared 目录结构

```powershell id="6sj25l"
cd D:\Untitled-OJ\services\shared

dir
```

不应看到：

```text id="or8kkb"
config
response
events
```

应看到：

```text id="zda1bz"
database
logger
middleware
security
tracing
go.mod
go.sum
```

---

## 十八、常见问题

### 18.1 go build 报找不到 ojos-shared/events

错误示例：

```text id="sc2sat"
package ojos-shared/events is not in std
```

原因：

```text id="hawvxc"
业务服务仍然 import 了旧 shared/events
```

解决：

```powershell id="xvfv79"
cd D:\Untitled-OJ\services

Get-ChildItem -Recurse -Include *.go |
  Select-String -Pattern "ojos-shared/events|events.NewBus|NewBusByURL"
```

找到后删除相关 import、字段、初始化逻辑和事件发布逻辑。

---

### 18.2 go build 报 missing go.sum entry for zipkin

错误示例：

```text id="n4d0x5"
missing go.sum entry for module providing package go.opentelemetry.io/otel/exporters/zipkin
```

原因：

```text id="52xsbg"
go-zero/core/trace 间接依赖 zipkin exporter
```

解决：

```powershell id="o6w9ui"
go get github.com/zeromicro/go-zero/core/trace@v1.10.2
go mod tidy
go build .
```

不要因为这个警告去手动删除 go.sum 里的 zipkin。

---

### 18.3 GoLand 提示 replace 本地路径不可移植

示例：

```text id="d23qce"
replace ojos-shared => ../shared 本地路径可能无法移植
```

当前可以忽略。

原因：

```text id="ph1wz0"
这是 monorepo 开发的正常 replace 写法
Docker build 也依赖当前 monorepo 相对路径
```

后续如果需要更好的本地开发体验，可以使用本地 `go.work`：

```powershell id="1g0015"
cd D:\Untitled-OJ

go work init `
  .\services\shared `
  .\services\auth `
  .\services\gateway `
  .\services\judge-api
```

但当前 `.gitignore` 中忽略 `go.work` 是合理的。

---

### 18.4 Permission deny 不生效

排查顺序：

```text id="wtcqvp"
1. 用户是否是 super_admin
2. permission_assignments 是否写入正确 principal_type / principal_id
3. permission_code 是否正确
4. scope_type / scope_id 是否正确
5. expires_at 是否已过期
6. 业务服务是否真的调用 RequireUserPermission
```

如果用户是：

```text id="rzv4gi"
super_admin
```

则 deny 不会生效，这是设计如此。

---

### 18.5 user_id 没写入 submission

排查：

```text id="yjgkbw"
1. Gateway 路由 AuthMode 是否 required
2. Gateway 是否注入 X-User-Id
3. judge-api 是否接入 UserContextMiddleware
4. judge-api 是否从 authctx.FromContext 读取用户
5. 请求是否走 Gateway，而不是直接打 judge-api
```

下游服务不应该信任请求体中的 `user_id`。

---

## 十九、后续规划

Shared 后续可以逐步补充：

```text id="k9mvu1"
统一错误码模块
统一 JSON 响应模块
authctx middleware 通用化
Redis helper
Redis Streams queue helper
OpenTelemetry Redis propagation
统一 shutdown helper
统一 service bootstrap helper
```

但当前不建议马上做太多抽象。

推荐顺序：

```text id="br08b7"
1. 统一错误响应
2. authctx middleware 通用化
3. Redis Streams helper
4. service shutdown helper
5. trace propagation for Redis Streams
```

不建议当前立刻加入：

```text id="5c2wcr"
通用 EventBus
复杂 Service Kernel
统一 Config Loader
过度封装的 App 框架
```

原因是业务模块边界还在演进，过早抽象会导致反复重构。

---

## 二十、当前结论

Shared 当前已经从旧的：

```text id="39r8b0"
配置加载 + NATS EventBus + response 包装 + 部分基础设施
```

演进为：

```text id="6oyiml"
Go 微服务公共基础库 + 安全上下文 + 权限核心
```

它当前承担的最重要职责是：

```text id="i29su1"
为所有 Go 服务提供稳定基础设施
为 Gateway / Auth / Judge API 提供安全和权限能力
保证 Permission Core 作为平台级能力复用
```

当前 Shared 的正确方向是：

```text id="okb0pv"
小而稳定
参数驱动
不碰业务
不提前抽象事件系统
优先服务现有模块
后续按重复需求逐步扩展
```

当前 Shared 已经可以支撑后续继续开发：

```text id="6h3xod"
Problem Core
Dataset Core
Contest Core
Scoreboard Core
Permission API
Module Registry
Launcher
```

但在新增这些模块时仍需坚持：

```text id="egxvf8"
业务逻辑进业务服务
通用能力进 Shared
不确定的抽象先不要进 Shared
```
