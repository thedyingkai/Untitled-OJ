# OJOS Gateway 模块开发文档

## 一、模块定位

`services/gateway` 是 OJOS 平台的统一 HTTP 入口服务。

它负责将外部请求统一接入 OJOS 内部微服务体系，并在请求进入业务服务之前完成通用入口层处理。

Gateway 当前主要职责包括：

```text
统一 HTTP 入口
健康检查
配置驱动反向代理
JWT 鉴权
用户上下文透传
可信 Header 注入
客户端伪造 Header 清理
基础日志记录
链路追踪接入
panic recovery
模块路由聚合
```

Gateway 的核心定位是：

```text
认证入口
流量入口
代理入口
上下文入口
```

而不是：

```text
业务服务
权限服务
题库服务
比赛服务
判题服务
```

Gateway 不应该直接理解复杂业务规则。

例如，Gateway 可以知道：

```text
/api/judge/* 需要登录
/api/auth/* 可以匿名访问
```

但 Gateway 不应该判断：

```text
用户能不能提交 problem:7
用户能不能编辑 contest:3
用户能不能管理 balloon:12
用户能不能安装 module:scoreboard-acm
```

这些属于业务服务和 Permission Core 的职责。

当前 Gateway 监听端口：

```text
8080
```

外部统一访问入口：

```text
http://localhost:8080
```

内部服务访问示例：

```text
/api/auth/login
    -> auth:8081/auth/login

/api/judge/submissions
    -> judge-api:8082/judge/submissions
```

---

## 二、当前版本状态

当前 Gateway 已完成 go-zero 重构，并完成配置驱动代理能力。

当前版本可以记为：

```text
Gateway v0.3+
```

当前已完成能力：

```text
go-zero 标准服务结构
gateway.api 定义自身接口
goctl 生成 handler / logic / types / routes
/health 健康检查
shared logger 接入
shared tracing 接入
shared database 接入
shared middleware 接入
JWT 鉴权
AuthMode 路由模式
可信用户上下文 Header 注入
客户端伪造 Header 清理
配置驱动反向代理
Auth 服务代理
Judge API 服务代理
ReverseProxy Rewrite 方式转发
Dockerfile 更新
Docker Compose 部署
go build . 通过
```

当前已删除或不再使用：

```text
旧 internal/app
旧 internal/router
旧手写 main.go
旧 shared/config
旧 shared/response
旧 shared/events
NATS EventBus
NatsConfig
nats://ojos-nats:4222
httputil.ReverseProxy.Director 写法
```

当前 Gateway 不再依赖 NATS。

如果 Gateway 代码或配置中仍出现：

```text
Nats
NatsConfig
events.NewBusByURL
ojos-shared/events
nats://ojos-nats:4222
4222
```

说明 NATS 清理没有完成。

---

## 三、当前目录结构

当前 Gateway 模块目录结构应为：

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
├── gateway.api
├── gateway.go
├── Dockerfile
├── go.mod
└── go.sum
```

说明：

```text
gateway.api      定义 Gateway 自身接口
gateway.go       go-zero 入口文件
handler          goctl 生成 HTTP handler
logic            goctl 生成业务逻辑层
types            goctl 生成请求 / 响应类型
svc              依赖注入和 ServiceContext
proxy            手写配置驱动反向代理模块
etc              服务配置
```

其中：

```text
internal/proxy/proxy.go
```

是 Gateway 的核心手写模块。它不由 goctl 生成。

Gateway 的 `/api/auth/*`、`/api/judge/*` 这类代理路由不应该写入 `gateway.api`。它们通过 `gateway.yaml` 中的 `Proxy.Routes` 配置动态接入。

这样设计的原因是：

```text
新增内部模块时，不需要每次修改 gateway.api
新增内部模块时，不需要每次重新生成 Gateway handler
Gateway 只需要配置路由即可代理新服务
后续 module-registry / launcher 可以自动写入或生成 Gateway 路由配置
```

---

## 四、go-zero 重构说明

Gateway 使用 go-zero API 模式。

初始生成方式可以是：

```powershell
cd D:\Untitled-OJ\services

goctl api new gateway --module ojos-gateway
```

当前实际接口文件为：

```text
services/gateway/gateway.api
```

修改 `gateway.api` 后重新生成：

```powershell
cd D:\Untitled-OJ\services\gateway

goctl api go -api gateway.api -dir . --style gozero
```

推荐使用项目脚本：

```powershell
cd D:\Untitled-OJ

.\scripts\gen-gozero.ps1 -Service gateway
```

或者生成所有 go-zero 服务：

```powershell
.\scripts\gen-gozero.ps1
```

注意：

```text
goctl 生成的 handler / logic / routes / types 是源码
应进入 Git
不应被 .gitignore 忽略
```

Gateway 当前不再使用旧结构：

```text
internal/app
internal/router
configs/config.yaml
main.go
```

当前标准入口是：

```text
gateway.go
```

如果项目中仍然存在旧结构，需要确认是否已经废弃或删除，避免新旧入口混淆。

---

## 五、gateway.api

路径：

```text
services/gateway/gateway.api
```

Gateway 的 `.api` 文件只定义 Gateway 自身接口。

当前最小内容为：

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

当前 Gateway 自身只需要：

```http
GET /health
```

代理路由不写在 `gateway.api` 中。

也就是说，不要在 `gateway.api` 中写：

```text
/api/auth/login
/api/auth/profile
/api/judge/submissions
/api/judge/submissions/:id
```

这些路径属于下游服务，由 Gateway 的配置驱动代理处理。

这样可以保证：

```text
Auth API 变动时，不需要重写 Gateway API
Judge API 变动时，不需要重写 Gateway API
Problem API 接入时，只需要改 gateway.yaml
Contest API 接入时，只需要改 gateway.yaml
Launcher 后续可以自动注册 Gateway Route
```

---

## 六、配置文件

路径：

```text
services/gateway/etc/gateway.yaml
```

当前推荐配置：

```yaml
Name: gateway-service
Host: 0.0.0.0
Port: 8080

Database:
  Url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

Jaeger:
  Endpoint: ojos-jaeger:4317

Jwt:
  Secret: ojos-dev-secret-change-me

Proxy:
  Routes:
    - Prefix: /api/auth
      Target: http://auth:8081
      StripPrefix: /api
      AuthMode: optional

    - Prefix: /api/judge
      Target: http://judge-api:8082
      StripPrefix: /api
      AuthMode: required
```

字段说明：

| 字段                | 说明                 |
| ----------------- | ------------------ |
| `Name`            | 服务名称，用于日志和 tracing |
| `Host`            | HTTP 监听地址          |
| `Port`            | HTTP 监听端口          |
| `Database.Url`    | PostgreSQL 连接地址    |
| `Jaeger.Endpoint` | OTLP gRPC endpoint |
| `Jwt.Secret`      | JWT 解析密钥           |
| `Proxy.Routes`    | 配置驱动代理路由           |

当前配置中不应再出现：

```yaml
Nats:
  Url: nats://ojos-nats:4222
```

如果仍然存在，应删除。

---

## 七、配置结构

路径：

```text
services/gateway/internal/config/config.go
```

当前推荐结构：

```go
package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Jaeger   JaegerConfig
    Jwt      JwtConfig
    Proxy    ProxyConfig
}

type DatabaseConfig struct {
    Url string
}

type JaegerConfig struct {
    Endpoint string
}

type JwtConfig struct {
    Secret string
}

type ProxyConfig struct {
    Routes []ProxyRouteConfig
}

type ProxyRouteConfig struct {
    Prefix      string
    Target      string
    StripPrefix string `json:",optional"`
    AuthMode    string `json:",optional"`
}
```

当前不应再存在：

```go
Nats NatsConfig
```

也不应再存在：

```go
type NatsConfig struct {
    Url string
}
```

Gateway 不再依赖：

```go
ojos-shared/config
```

也不再使用：

```go
config.Load()
```

每个 go-zero 服务自己定义配置结构，Shared 只提供基础设施初始化函数。

---

## 八、ServiceContext

路径：

```text
services/gateway/internal/svc/servicecontext.go
```

`ServiceContext` 是 Gateway 的依赖注入中心。

当前推荐结构：

```go
type ServiceContext struct {
    Config config.Config

    Logger *zap.Logger
    DB     *pgxpool.Pool
    Tracer *sdktrace.TracerProvider

    ProxyHandler http.HandlerFunc
}
```

实际字段可以按当前代码保留，但不应再包含：

```go
Bus *events.Bus
```

也不应再初始化：

```go
events.NewBusByURL(c.Nats.Url, c.Name)
```

Gateway 初始化流程推荐为：

```text
context.Background()
    ↓
logger.New(c.Name)
    ↓
tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
    ↓
database.NewPostgresPoolByURL(ctx, c.Database.Url)
    ↓
proxy.NewConfigProxy(c.Proxy.Routes, log, jwt config)
    ↓
return ServiceContext
```

如果当前 Gateway 暂时直接使用 `pgxpool.New` 而不是 `shared/database`，也可以运行，但推荐统一为 Shared 方式。

---

### 8.1 为什么 Gateway 当前仍连接数据库

Gateway 当前连接 PostgreSQL 的必要性不是特别强。

保留数据库连接主要是为了：

```text
验证 shared/database 链路
为后续 Gateway 级配置 / feature flag / route registry 做准备
可能用于 module-registry 后的动态路由读取
```

但当前 Gateway 的核心代理逻辑并不强依赖数据库。

后续有两种选择：

```text
继续保留 DB 连接，供 module-registry / feature-flag 使用
如果短期不用动态配置，可以从 Gateway 中删除 DB 连接，减少依赖
```

当前为了保持与 Shared 和可观测性链路一致，可以先保留。

---

### 8.2 关闭流程

go-zero 默认生成的入口不一定显式处理 shutdown。

后续建议为 Gateway 的 `ServiceContext` 增加：

```go
func (s *ServiceContext) Close(ctx context.Context) error
```

关闭：

```text
DB pool
TracerProvider
Logger Sync
```

示例：

```go
func (s *ServiceContext) Close(ctx context.Context) error {
    if s.DB != nil {
        s.DB.Close()
    }

    if s.Tracer != nil {
        _ = s.Tracer.Shutdown(ctx)
    }

    if s.Logger != nil {
        _ = s.Logger.Sync()
    }

    return nil
}
```

当前没有 Close 不影响 MVP，但后续应该补齐。

---

## 九、配置驱动代理设计

Gateway 的核心设计是配置驱动代理。

示例配置：

```yaml
Proxy:
  Routes:
    - Prefix: /api/auth
      Target: http://auth:8081
      StripPrefix: /api
      AuthMode: optional

    - Prefix: /api/judge
      Target: http://judge-api:8082
      StripPrefix: /api
      AuthMode: required
```

含义：

```text
Prefix      用于匹配外部请求路径
Target      内部服务地址
StripPrefix 转发前从外部路径中删除的前缀
AuthMode    鉴权模式
```

---

### 9.1 Auth 转发规则

外部请求：

```http
POST /api/auth/login
```

匹配：

```text
Prefix = /api/auth
```

去前缀：

```text
StripPrefix = /api
```

转发路径：

```text
/auth/login
```

最终转发为：

```http
POST http://auth:8081/auth/login
```

---

### 9.2 Judge 转发规则

外部请求：

```http
POST /api/judge/submissions
```

匹配：

```text
Prefix = /api/judge
```

去前缀：

```text
StripPrefix = /api
```

转发路径：

```text
/judge/submissions
```

最终转发为：

```http
POST http://judge-api:8082/judge/submissions
```

---

### 9.3 后续 Problem API 转发规则

未来新增：

```text
services/problem-api
```

可以只增加配置：

```yaml
Proxy:
  Routes:
    - Prefix: /api/problem
      Target: http://problem-api:8083
      StripPrefix: /api
      AuthMode: required
```

则：

```text
/api/problem/problems
    -> http://problem-api:8083/problem/problems
```

是否使用 `/api/problem` 还是 `/api/problems`，后续可以统一 API 命名规范。

---

### 9.4 路由匹配顺序

Gateway 应按 Prefix 长度从长到短匹配。

例如同时存在：

```text
/api
/api/auth
/api/auth/admin
```

应优先匹配：

```text
/api/auth/admin
```

而不是先匹配 `/api`。

因此代理模块中应该对 routes 进行排序：

```text
len(prefix) desc
```

这样可以避免短前缀吞掉长前缀。

---

## 十、proxy 模块

路径：

```text
services/gateway/internal/proxy/proxy.go
```

该模块是 Gateway 的核心手写模块。

主要职责：

```text
读取 Proxy.Routes
校验 Prefix
校验 Target
解析 Target URL
清理 Prefix / StripPrefix
按 Prefix 长度排序
根据请求 path 匹配路由
执行 AuthMode 检查
重写 upstream path
注入转发 Header
注入 trace context
调用 ReverseProxy
处理 upstream 错误
```

---

### 10.1 cleanPrefix

`cleanPrefix` 用于规范化前缀。

目标：

```text
auth     -> /auth
/api/    -> /api
/        -> /
空字符串 -> 空字符串
```

规则：

```text
去除首尾空格
如果非空且不以 / 开头，则补 /
如果长度大于 1，则去掉结尾 /
```

这样可以避免配置中出现：

```text
api/auth
/api/auth/
/api/auth
```

导致匹配行为不一致。

---

### 10.2 matchPrefix

路由匹配不能简单使用：

```go
strings.HasPrefix(path, prefix)
```

否则：

```text
/api/auth2
```

会错误匹配：

```text
/api/auth
```

正确逻辑应为：

```go
return path == prefix || strings.HasPrefix(path, prefix+"/")
```

这样：

```text
/api/auth/login
```

可以匹配 `/api/auth`，但：

```text
/api/auth2/login
```

不会匹配 `/api/auth`。

---

### 10.3 路径重写

假设：

```text
Target = http://auth:8081
StripPrefix = /api
OriginalPath = /api/auth/login
```

则：

```text
upstreamPath = /auth/login
```

如果 Target 本身有 path，例如：

```text
Target = http://service:8080/base
```

则最终路径应正确拼接为：

```text
/base/auth/login
```

需要处理：

```text
target path 是否以 / 结尾
upstream path 是否以 / 开头
```

避免出现：

```text
//auth/login
baseauth/login
```

---

### 10.4 ReverseProxy Rewrite

Go 新版 `httputil.ReverseProxy` 推荐使用 `Rewrite`，而不是旧的 `Director`。

旧写法：

```go
rp := httputil.NewSingleHostReverseProxy(target)
originDirector := rp.Director

rp.Director = func(req *http.Request) {
    originDirector(req)
    ...
}
```

GoLand 会提示：

```text
Director 已弃用
```

当前应改成：

```go
rp := &httputil.ReverseProxy{
    Rewrite: func(pr *httputil.ProxyRequest) {
        ...
    },
}
```

在 `Rewrite` 中应使用：

```go
pr.SetURL(target)
pr.Out.URL.Path = ...
pr.Out.Host = target.Host
```

还要注意：

```text
pr.In  是原始请求
pr.Out 是转发请求
```

可信 Header 应写入 `pr.Out.Header`。

---

### 10.5 ErrorHandler

当 upstream 服务不可用时，Gateway 应返回 Bad Gateway。

推荐响应：

```json
{
  "code": 50201,
  "msg": "bad gateway"
}
```

当前如果统一错误响应尚未完成，也至少应该：

```text
设置 Content-Type: application/json; charset=utf-8
返回 HTTP 502
写入简单 JSON
记录错误日志
```

日志字段建议包含：

```text
method
path
target
error
trace_id
span_id
```

---

## 十一、AuthMode 设计

Gateway 当前支持三种 AuthMode：

```text
none
optional
required
```

推荐含义：

| AuthMode   | 含义                       |
| ---------- | ------------------------ |
| `none`     | 完全不解析 token              |
| `optional` | 有 token 就解析，没有 token 也放行 |
| `required` | 必须提供合法 token             |

---

### 11.1 none

适用于：

```text
公开静态资源
公开健康检查
不需要用户上下文的公开接口
```

行为：

```text
不读取 Authorization
不注入用户上下文 Header
直接转发
```

---

### 11.2 optional

适用于：

```text
登录接口
注册接口
公开题目列表但可识别登录用户
公开榜单但登录用户可看到个性化信息
```

行为：

```text
没有 Authorization -> 放行
有 Authorization 且合法 -> 注入用户上下文
有 Authorization 但非法 -> 建议返回 401
```

当前 `/api/auth` 可以使用 optional，因为：

```text
/register 和 /login 不需要登录
/profile 在 Auth 服务内部由 AuthMiddleware 保护
```

---

### 11.3 required

适用于：

```text
提交代码
查看自己的提交
创建题目
管理比赛
后台接口
```

行为：

```text
必须有 Authorization
必须是 Bearer token
token 必须合法
token 未过期
解析成功后注入可信用户上下文
```

当前 `/api/judge` 使用：

```text
AuthMode: required
```

因此所有 Judge API 请求都要求登录。

后续如果希望：

```text
GET /api/judge/submissions/:id
```

允许公开访问，就需要更细粒度路由，而不是整个 `/api/judge` 都 required。这可以在后续 Gateway Route 设计中支持。

---

## 十二、JWT 验证

Gateway 使用：

```text
services/shared/security/jwt
```

解析 JWT。

JWT Secret 来自：

```yaml
Jwt:
  Secret: ojos-dev-secret-change-me
```

Auth 签发 token 和 Gateway 解析 token 必须使用同一个 secret。

如果出现：

```text
Auth 登录成功
Gateway 访问 protected API 返回 invalid token
```

优先检查：

```text
auth.yaml Jwt.Secret
gateway.yaml Jwt.Secret
```

是否一致。

---

### 12.1 Authorization Header

请求头格式：

```text
Authorization: Bearer <token>
```

常见错误：

```text
缺少 Authorization
Authorization 不是 Bearer 格式
token 为空
token 过期
签名错误
claims 无效
```

推荐错误响应：

```json
{
  "code": 40101,
  "msg": "missing authorization header"
}
```

或：

```json
{
  "code": 40102,
  "msg": "invalid token"
}
```

当前统一错误响应尚未完成，后续应统一处理。

---

### 12.2 Claims

Gateway 解析出的 Claims 至少包括：

```text
user_id
username
roles
exp
iat
iss
sub
```

Gateway 不应该根据 roles 做复杂权限判断。

Gateway 只需要将基础用户上下文转发给下游服务。

---

## 十三、可信用户上下文透传

Gateway 验证 JWT 后，会向下游服务注入可信 Header。

推荐 Header：

```text
X-Auth-Verified: true
X-User-Id: <user_id>
X-Username: <username>
X-Roles: <role1,role2>
```

下游服务通过这些 Header 建立用户上下文。

例如 judge-api 的流程：

```text
Gateway 验证 JWT
    ↓
Gateway 注入 X-User-Id
    ↓
judge-api UserContextMiddleware 读取 Header
    ↓
authctx.WithContext 写入 context
    ↓
CreateSubmissionLogic 从 authctx.FromContext 读取 user_id
    ↓
submissions.user_id = user_id
```

---

### 13.1 清理客户端伪造 Header

Gateway 在注入可信 Header 之前，必须删除客户端传来的同名 Header。

必须清理：

```text
X-Auth-Verified
X-User-Id
X-Username
X-Roles
```

原因：

```text
如果不清理，客户端可以直接伪造 X-User-Id
绕过身份绑定
替别人提交代码
查看别人的资源
```

正确流程：

```text
删除客户端伪造 Header
    ↓
解析 JWT
    ↓
用 JWT Claims 重新设置 Header
```

下游服务只应信任 Gateway 注入后的 Header。

---

### 13.2 Header 编码

`X-Roles` 推荐使用逗号分隔：

```text
X-Roles: user,admin
```

下游服务解析时：

```text
按逗号分割
去除空格
过滤空字符串
```

后续如果 roles 复杂，可以改为：

```text
X-Roles-Json
```

但当前没必要。

---

## 十四、Gateway 与 Permission Core 的边界

Gateway 不做资源级权限判断。

这是当前架构中非常重要的边界。

错误设计：

```text
Gateway 判断用户是否可以提交 problem:7
Gateway 判断用户是否可以编辑 contest:3
Gateway 判断用户是否可以发 balloon
Gateway 判断用户是否可以安装 module
```

正确设计：

```text
Gateway 只判断是否登录
业务服务判断具体权限
```

例如：

```text
POST /api/judge/submissions
    Gateway:
        required auth
        注入 user_id

    judge-api:
        RequireUserPermission(user_id, "judge.submit", system:0)
```

未来：

```text
POST /api/problem/problems
    Gateway:
        required auth
        注入 user_id

    problem-api:
        RequireUserPermission(user_id, "problem.create", system:0)
```

未来：

```text
POST /api/contest/contests/:id/freeze
    Gateway:
        required auth
        注入 user_id

    contest-api:
        RequireUserPermission(user_id, "contest.freeze", contest:id)
```

这样设计的原因：

```text
Gateway 不理解业务资源关系
Gateway 不应该查询 problem / contest / scoreboard 业务表
Gateway 不应该耦合所有模块权限点
新增模块不应该改 Gateway 代码
```

Gateway 的权限边界是：

```text
认证，不是授权
```

---

## 十五、Gateway 与 Auth 的关系

Auth 是身份提供者。

Gateway 使用 Auth 签发的 JWT 完成入口鉴权。

关系：

```text
Auth:
    注册用户
    登录用户
    签发 JWT

Gateway:
    解析 JWT
    检查 token 是否有效
    注入用户上下文
    代理请求到下游服务
```

Gateway 不调用 Auth 服务去校验每次请求。

原因：

```text
JWT 是自包含 token
每个请求都调用 Auth 会增加延迟
Auth 挂掉会导致所有已登录请求不可用
```

因此 Gateway 只需要：

```text
共享 JWT Secret
使用 shared/security/jwt 解析 token
```

---

## 十六、Gateway 与 Judge API 的关系

当前 Judge API 通过 Gateway 暴露：

```text
/api/judge/*
```

Gateway 配置：

```yaml
- Prefix: /api/judge
  Target: http://judge-api:8082
  StripPrefix: /api
  AuthMode: required
```

因此：

```text
POST /api/judge/submissions
    -> POST /judge/submissions
```

所有 `/api/judge/*` 请求都必须登录。

当前 Judge API 会使用 Gateway 注入的用户上下文。

因此不要直接在业务测试中绕过 Gateway 调用：

```text
http://localhost:8082/judge/submissions
```

除非你手动注入可信 Header。

正常测试应使用：

```text
http://localhost:8080/api/judge/submissions
```

这样才能验证完整链路：

```text
Auth 登录
Gateway JWT
Gateway Header 注入
judge-api 用户上下文
Permission Core
Redis Stream
Judge Worker
```

---

## 十七、Gateway 与 Redis Streams 的关系

Gateway 当前不直接操作 Redis Streams。

Redis Streams Judge Queue 的链路是：

```text
judge-api
    ↓
Redis Stream ojos:judge:submissions
    ↓
judge-worker
```

Gateway 只负责把用户请求转发给 judge-api。

因此 Gateway 不应包含：

```text
Redis XADD
XREADGROUP
XACK
Judge Stream 逻辑
```

如果后续 Gateway 需要 Redis，一般是为了：

```text
限流
会话
缓存
动态配置
```

而不是为了判题队列。

---

## 十八、Gateway 与 Module Registry 的未来关系

当前 Gateway 的路由来自静态配置：

```text
gateway.yaml
```

未来如果实现：

```text
module-registry
launcher
feature-flag-core
```

Gateway 路由可能从以下来源生成：

```text
模块安装清单
模块注册表
数据库 module_routes
配置文件渲染
动态 reload
```

未来每个模块可能提供：

```text
ojos.module.yaml
```

其中声明：

```yaml
routes:
  - prefix: /api/problem
    target: http://problem-api:8083
    strip_prefix: /api
    auth_mode: required
```

Launcher 安装模块时：

```text
读取 module manifest
检查依赖模块
注册权限点
注册资源类型
注册 Gateway 路由
启动服务或更新 compose
刷新 Gateway 配置
```

当前还没有实现动态路由注册，所以仍使用 `gateway.yaml` 静态配置。

---

## 十九、错误处理

Gateway 当前至少需要处理三类错误。

### 19.1 认证错误

例如：

```text
缺少 Authorization
Bearer token 格式错误
token 无效
token 过期
```

推荐 HTTP 状态：

```text
401
```

推荐响应：

```json
{
  "code": 40101,
  "msg": "missing authorization header"
}
```

或：

```json
{
  "code": 40102,
  "msg": "invalid token"
}
```

---

### 19.2 代理错误

例如：

```text
auth 服务不可用
judge-api 服务不可用
upstream connection refused
upstream timeout
```

推荐 HTTP 状态：

```text
502
```

推荐响应：

```json
{
  "code": 50201,
  "msg": "bad gateway"
}
```

---

### 19.3 Gateway 内部错误

例如：

```text
panic
配置错误
URL parse 错误
初始化失败
```

初始化阶段配置错误应直接：

```text
log.Fatalf
```

运行时 panic 应由 recovery middleware 捕获。

推荐 HTTP 状态：

```text
500
```

推荐响应：

```json
{
  "code": 50001,
  "msg": "internal server error"
}
```

当前统一错误响应尚未完成，后续应将 Gateway、Auth、Judge API 的错误格式统一。

---

## 二十、日志与 tracing

Gateway 是入口服务，因此它的日志对排查问题非常关键。

每个请求日志建议包含：

```text
method
path
status
duration
remote_addr
user_agent
trace_id
span_id
auth_mode
user_id
upstream
```

反向代理失败日志建议包含：

```text
method
path
target
error
trace_id
span_id
```

JWT 失败日志建议包含：

```text
method
path
reason
trace_id
span_id
```

但不要记录完整 token。

---

### 20.1 不要打印敏感信息

Gateway 日志不应打印：

```text
Authorization 完整内容
JWT token
Jwt.Secret
密码
数据库密码
```

可以打印：

```text
token missing
token invalid
token expired
```

但不要把 token 原文打出来。

---

### 20.2 trace context 转发

Gateway 代理到下游服务时应传播 OpenTelemetry trace context。

推荐注入：

```go
otel.GetTextMapPropagator().Inject(
    req.Context(),
    propagation.HeaderCarrier(req.Header),
)
```

在 ReverseProxy `Rewrite` 模式中，应注入到：

```go
pr.Out.Header
```

这样下游服务可以接续同一条 trace。

---

## 二十一、Docker Compose

Gateway 在 Compose 中应类似：

```yaml
gateway:
  build:
    context: ../../services
    dockerfile: gateway/Dockerfile
  container_name: ojos-gateway
  depends_on:
    postgres:
      condition: service_healthy
    auth:
      condition: service_started
    judge-api:
      condition: service_started
    jaeger:
      condition: service_started
  ports:
    - "8080:8080"
```

是否依赖 `auth` 和 `judge-api` 可以按实际 compose 调整。

但不应再依赖：

```yaml
nats:
  condition: service_started
```

不应再有：

```yaml
NATS_URL: nats://ojos-nats:4222
```

当前基础设施应为：

```text
PostgreSQL
Redis
Jaeger
Gateway
Auth
Judge API
Judge Worker
```

不再需要 NATS。

---

## 二十二、验收命令

### 22.1 Health

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/health"
```

预期：

```text
status = ok
```

---

### 22.2 Auth Login 代理

```powershell
$body = @{
  username = "permtest"
  password = "123456"
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/login" `
  -ContentType "application/json" `
  -Body $body

$res
```

预期返回 token。

如果响应格式为统一包装：

```powershell
$token = $res.data.token
```

如果响应直接返回：

```powershell
$token = $res.token
```

---

### 22.3 Auth Profile 代理

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期包含：

```text
user_id
username
email
roles
```

---

### 22.4 Judge 不带 token 应失败

```powershell
Invoke-WebRequest `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/1"
```

预期：

```text
401
```

如果当前返回格式未统一，后续统一错误响应阶段修正。

---

### 22.5 Judge 带 token 应通过

```powershell
$code = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    int a, b;
    cin >> a >> b;
    cout << a + b << endl;
    return 0;
}
'@

$body = @{
  problem_id = 1
  language = "cpp17"
  code = $code
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/submissions" `
  -ContentType "application/json" `
  -Headers @{ Authorization = "Bearer $token" } `
  -Body $body

$res
```

预期：

```text
submission_id = 新 ID
status = PENDING
```

这条链路验证了：

```text
Gateway JWT required
Gateway Header 注入
judge-api 用户上下文读取
Permission Core judge.submit
Redis Stream XADD
Judge Worker 消费
```

---

## 二十三、NATS 清理检查

Gateway 不应再包含任何 NATS 残留。

检查命令：

```powershell
cd D:\Untitled-OJ\services\gateway

Get-ChildItem -Recurse -Include *.go,*.yaml,go.mod,go.sum |
  Select-String -Pattern "nats|NATS|Nats|4222|ojos-shared/events|NewBusByURL"
```

预期：

```text
无输出
```

全项目检查：

```powershell
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期：

```text
无输出
```

注意：

```text
event-listener 不是 NATS
zipkin 不是 NATS
```

不要误删。

---

## 二十四、常见问题

### 24.1 /api/auth/login 返回 404

排查：

```text
gateway.yaml 中是否有 /api/auth 路由
Target 是否是 http://auth:8081
StripPrefix 是否是 /api
Auth 服务是否启动
Auth 内部路径是否是 /auth/login
```

正确配置：

```yaml
- Prefix: /api/auth
  Target: http://auth:8081
  StripPrefix: /api
  AuthMode: optional
```

---

### 24.2 /api/judge/submissions 返回 401

排查：

```text
是否传 Authorization Header
是否是 Bearer token
token 是否过期
Jwt.Secret 是否和 auth 一致
/api/judge 是否配置 required
```

---

### 24.3 /api/judge/submissions 返回 forbidden

这通常不是 Gateway 错误，而是 judge-api 权限检查失败。

排查：

```text
用户是否拥有 judge.submit
用户是否被 permission_assignments deny
用户是否是 super_admin
Permission Core 表是否初始化
judge-api 是否正确读取 user_id
```

---

### 24.4 下游 user_id 为空

排查 Gateway：

```text
JWT 是否解析成功
是否设置 X-Auth-Verified
是否设置 X-User-Id
是否清理后重新注入 Header
ReverseProxy Rewrite 是否把 Header 写到 pr.Out
```

排查下游：

```text
UserContextMiddleware 是否启用
是否读取 X-User-Id
是否写入 authctx
logic 是否从 authctx.FromContext 读取
```

---

### 24.5 GoLand 提示 Director deprecated

原因：

```text
httputil.ReverseProxy.Director 旧写法被提示弃用
```

解决：

```text
改用 ReverseProxy.Rewrite
```

不要继续使用：

```go
originDirector := rp.Director
rp.Director = func(req *http.Request) { ... }
```

推荐改为：

```go
rp := &httputil.ReverseProxy{
    Rewrite: func(pr *httputil.ProxyRequest) {
        ...
    },
}
```

---

### 24.6 Gateway 启动时报 connect postgres failed

排查：

```text
PostgreSQL 容器是否启动
compose 中 postgres service 名是否正确
容器内 Database.Url 是否使用 postgres:5432
宿主机 migrate 才使用 localhost:5433
```

容器内正确：

```text
postgres://postgres:password@postgres:5432/ojos?sslmode=disable
```

宿主机迁移正确：

```text
postgres://postgres:password@localhost:5433/ojos?sslmode=disable
```

---

## 二十五、安全注意事项

### 25.1 不信任客户端 Header

客户端传来的这些 Header 必须清理：

```text
X-Auth-Verified
X-User-Id
X-Username
X-Roles
```

Gateway 必须基于 JWT 重新生成它们。

---

### 25.2 不打印 token

日志中不要打印：

```text
Authorization
JWT token
Jwt.Secret
```

---

### 25.3 不在 Gateway 中硬编码业务权限

不要写：

```text
if path == "/api/judge/submissions" check judge.submit
```

应由 judge-api 检查。

---

### 25.4 AuthMode 默认值

如果某条 route 没写 AuthMode，推荐默认：

```text
required
```

或者在配置加载时强制要求写清楚。

当前为了兼容，也可以默认：

```text
none
```

但更安全的长期策略是：

```text
未配置 AuthMode 直接启动失败
```

这样避免新增模块时忘记配置鉴权。

---

## 二十六、后续规划

Gateway 后续可以扩展：

```text
统一错误响应
请求限流
IP 限流
用户级限流
Upstream timeout
熔断
重试策略
动态路由 reload
从 module-registry 加载路由
从 feature-flag-core 判断模块启停
WebSocket 代理
SSE 代理
静态资源代理
CORS 配置
请求 ID
审计日志
API 版本管理
```

推荐优先级：

```text
1. 统一错误响应
2. ReverseProxy Rewrite 完整替代 Director
3. 路由 AuthMode 默认安全策略
4. Upstream timeout
5. CORS 配置
6. module-registry 路由注册
```

当前不建议立刻做复杂动态服务发现。

原因：

```text
服务数量还少
模块边界还在稳定
静态 gateway.yaml 更容易调试
过早动态化会增加故障点
```

---

## 二十七、当前结论

Gateway 当前已经从旧的：

```text
手写 app/router
shared/config
shared/events
NATS EventBus
简单 /health 服务
```

演进为：

```text
go-zero 标准 Gateway
配置驱动代理
JWT 鉴权
可信用户上下文透传
AuthMode 路由策略
Shared 基础设施接入
NATS 清理完成后的统一 HTTP 入口
```

Gateway 当前最重要的价值是：

```text
统一外部入口
隐藏内部服务端口
统一认证方式
统一用户上下文传递
让业务服务专注业务权限
```

后续所有新增 HTTP 服务都应优先通过 Gateway 暴露。

当前已经适合继续接入：

```text
problem-api
contest-api
scoreboard-api
permission-api
module-registry
launcher
```

但在接入前，需要保持原则：

```text
Gateway 只管入口
业务服务管权限
Permission Core 管授权
Module Registry 管模块
Feature Flag 管启停
```
