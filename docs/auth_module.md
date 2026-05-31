# OJOS Auth 模块开发文档

## 一、模块定位

`services/auth` 是 OJOS 平台的认证服务，负责用户注册、登录、JWT 签发和用户身份解析。

当前 Auth 已完成 go-zero 重构，不再使用旧的手写 `main/app/router` 结构。

当前监听端口：

```text
8081
```

当前通过 Gateway 暴露：

```text
/api/auth/*
```

内部服务真实路径：

```text
/auth/*
```

---

## 二、当前完成状态

当前 Auth 已完成：

```text
go-zero 标准结构
auth.api 定义接口
goctl 生成 handler / logic / types / middleware
注册接口
登录接口
用户信息接口
健康检查接口
JWT 中间件
shared/database 接入
shared/events 接入
shared/logger 接入
shared/tracing 接入
shared/middleware 接入
Dockerfile 更新
go build . 通过
```

当前 Auth 可以记为：

```text
Auth go-zero 重构 v0.2 完成
```

---

## 三、目录结构

当前目录结构：

```text
services/auth/

├── etc/
│   └── auth.yaml
│
├── internal/
│   ├── config/
│   │   └── config.go
│   │
│   ├── handler/
│   │   ├── healthhandler.go
│   │   ├── loginhandler.go
│   │   ├── profilehandler.go
│   │   ├── registerhandler.go
│   │   └── routes.go
│   │
│   ├── logic/
│   │   ├── healthlogic.go
│   │   ├── loginlogic.go
│   │   ├── profilelogic.go
│   │   └── registerlogic.go
│   │
│   ├── middleware/
│   │   └── authmiddleware.go
│   │
│   ├── repository/
│   │   └── user_repository.go
│   │
│   ├── service/
│   │   └── auth_service.go
│   │
│   ├── svc/
│   │   └── servicecontext.go
│   │
│   ├── token/
│   │   └── token.go
│   │
│   └── types/
│       └── types.go
│
├── auth.api
├── auth.go
├── Dockerfile
├── go.mod
└── go.sum
```

已删除或不再使用的旧结构：

```text
internal/app
internal/router
旧手写 main.go
旧 shared/config 依赖
旧 shared/response 依赖
```

---

## 四、go-zero 重构说明

Auth 使用 go-zero API 模式。

生成方式：

```powershell
cd D:\Untitled-OJ\services

goctl api new auth --module ojos-auth
```

修改 `auth.api` 后重新生成：

```powershell
cd D:\Untitled-OJ\services\auth

goctl api go -api auth.api -dir .
```

如果出现默认模板残留，例如：

```text
internal/logic/authlogic.go
internal/handler/authhandler.go
```

并且其中引用：

```go
types.Request
types.Response
```

应删除这些旧文件：

```powershell
Remove-Item internal\logic\authlogic.go -Force
Remove-Item internal\handler\authhandler.go -Force
```

---

## 五、auth.api

路径：

```text
services/auth/auth.api
```

当前接口：

```text
GET  /health
POST /auth/register
POST /auth/login
GET  /auth/profile
```

其中 `/auth/profile` 使用 go-zero middleware：

```text
AuthMiddleware
```

接口分组：

```go
service auth {
    @handler health
    get /health returns (HealthResp)
}

@server(
    prefix: /auth
)
service auth {
    @handler register
    post /register (RegisterReq) returns (RegisterResp)

    @handler login
    post /login (LoginReq) returns (LoginResp)
}

@server(
    prefix: /auth
    middleware: AuthMiddleware
)
service auth {
    @handler profile
    get /profile returns (ProfileResp)
}
```

---

## 六、配置文件

路径：

```text
services/auth/etc/auth.yaml
```

当前配置：

```yaml
Name: auth-service
Host: 0.0.0.0
Port: 8081

Database:
  Url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

Nats:
  Url: nats://ojos-nats:4222

Jaeger:
  Endpoint: ojos-jaeger:4317

Jwt:
  Secret: ojos-dev-secret-change-me
  ExpireHours: 24
```

字段说明：

| 字段                | 说明                 |
| ----------------- | ------------------ |
| `Name`            | 服务名称               |
| `Host`            | HTTP 监听地址          |
| `Port`            | HTTP 监听端口          |
| `Database.Url`    | PostgreSQL 连接地址    |
| `Nats.Url`        | NATS 连接地址          |
| `Jaeger.Endpoint` | OTLP gRPC endpoint |
| `Jwt.Secret`      | JWT 签名密钥           |
| `Jwt.ExpireHours` | JWT 过期时间，单位小时      |

---

## 七、配置结构

路径：

```text
services/auth/internal/config/config.go
```

当前结构：

```go
type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Nats     NatsConfig
    Jaeger   JaegerConfig
    Jwt      JwtConfig
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

type JwtConfig struct {
    Secret      string
    ExpireHours int
}
```

Auth 不再依赖：

```go
ojos-shared/config
```

也不再使用：

```go
config.Load()
```

---

## 八、ServiceContext

路径：

```text
services/auth/internal/svc/servicecontext.go
```

`ServiceContext` 负责初始化 Auth 的全部依赖。

当前包含：

```go
type ServiceContext struct {
    Config config.Config

    Logger *zap.Logger
    DB     *pgxpool.Pool
    Tracer *sdktrace.TracerProvider
    Bus    *events.Bus

    UserRepo    *repository.UserRepository
    AuthService *service.AuthService

    AuthMiddleware rest.Middleware
}
```

初始化内容：

```text
zap logger
OpenTelemetry tracer
PostgreSQL pool
NATS bus
UserRepository
AuthService
AuthMiddleware
```

关闭流程：

```text
关闭 NATS
关闭 PostgreSQL pool
关闭 TracerProvider
Sync Logger
```

---

## 九、Repository 层

路径：

```text
services/auth/internal/repository/user_repository.go
```

Repository 负责直接访问数据库。

主要职责：

```text
根据 username 查询用户
根据 user_id 查询角色
创建用户
分配默认角色 user
查询用户角色
```

当前依赖：

```go
*pgxpool.Pool
```

典型构造函数：

```go
func NewUserRepository(db *pgxpool.Pool) *UserRepository
```

---

## 十、Service 层

路径：

```text
services/auth/internal/service/auth_service.go
```

Service 层负责认证业务逻辑。

主要职责：

```text
注册用户
密码哈希
校验密码
登录签发 JWT
查询用户角色
发布相关事件
```

### 10.1 Register

当前调用形式：

```go
result, err := l.svcCtx.AuthService.Register(
    l.ctx,
    service.RegisterRequest{
        Username: req.Username,
        Email:    req.Email,
        Password: req.Password,
    },
)
```

`RegisterRequest` 结构由 service 包定义。

### 10.2 Login

当前调用形式：

```go
result, err := l.svcCtx.AuthService.Login(
    l.ctx,
    service.LoginRequest{
        Username: req.Username,
        Password: req.Password,
    },
)
```

`LoginRequest` 结构由 service 包定义。

### 10.3 事件发布

AuthService 可以通过 Shared EventBus 发布事件，例如：

```text
user.registered
user.login
```

事件结构由 shared/events 统一生成。

---

## 十一、Token 模块

路径：

```text
services/auth/internal/token/token.go
```

Token 模块负责 JWT 签发和解析。

核心职责：

```text
定义 Claims
生成 JWT
解析 JWT
验证过期时间
保存 user_id / username / roles
```

Claims 中至少应包含：

```text
user_id
username
roles
iss
sub
exp
iat
```

`AuthMiddleware` 会调用 token 模块解析 Bearer Token。

---

## 十二、AuthMiddleware

路径：

```text
services/auth/internal/middleware/authmiddleware.go
```

AuthMiddleware 用于保护需要登录的接口。

当前保护：

```text
GET /auth/profile
```

处理流程：

```text
读取 Authorization Header
检查是否存在
检查是否 Bearer 前缀
提取 token
解析 JWT
失败返回业务错误
成功把 claims 写入 request context
执行下游 handler
```

错误响应示例：

```json
{
  "code": 40101,
  "msg": "missing authorization header"
}
```

可能错误码：

| 错误码     | 含义                        |
| ------- | ------------------------- |
| `40101` | 缺少 Authorization Header   |
| `40102` | Authorization Header 格式错误 |
| `40103` | Token 为空                  |
| `40104` | Token 无效或过期               |
| `40105` | 未授权                       |

Claims 读取函数：

```go
func ClaimsFromContext(ctx context.Context) (*token.Claims, bool)
```

---

## 十三、Logic 层

### 13.1 Health

路径：

```text
internal/logic/healthlogic.go
```

响应：

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

### 13.2 Register

路径：

```text
internal/logic/registerlogic.go
```

流程：

```text
接收 username / email / password
调用 AuthService.Register
成功返回 user_id / username
失败返回 code 和错误信息
```

响应示例：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "user_id": 1,
    "username": "admin"
  }
}
```

### 13.3 Login

路径：

```text
internal/logic/loginlogic.go
```

流程：

```text
接收 username / password
调用 AuthService.Login
校验密码
签发 JWT
返回 token / user_id / username / roles
```

响应示例：

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

登录失败：

```json
{
  "code": 40012,
  "msg": "invalid username or password"
}
```

### 13.4 Profile

路径：

```text
internal/logic/profilelogic.go
```

流程：

```text
从 context 读取 JWT Claims
返回 user_id / username / roles
```

响应示例：

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

## 十四、auth.go 启动流程

路径：

```text
services/auth/auth.go
```

启动流程：

```text
解析 -f 参数
加载 auth.yaml
初始化 ServiceContext
创建 go-zero rest server
注册 shared RecoveryMiddleware
注册 shared LoggingMiddleware
注册 goctl handlers
启动 HTTP Server
```

Auth 使用 Shared middleware：

```go
server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
server.Use(sharedmw.LoggingMiddleware(svcCtx.Logger, svcCtx.Tracer))
```

---

## 十五、Dockerfile

路径：

```text
services/auth/Dockerfile
```

当前内容：

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

说明：

```text
Auth 依赖 Shared
Docker build context 必须是 ../../services
必须同时复制 auth 和 shared
```

---

## 十六、Docker Compose

Auth 的 Compose 配置应类似：

```yaml
auth:
  build:
    context: ../../services
    dockerfile: auth/Dockerfile
  container_name: ojos-auth
  depends_on:
    postgres:
      condition: service_healthy
    nats:
      condition: service_started
    jaeger:
      condition: service_started
  ports:
    - "8081:8081"
```

Gateway 中代理 Auth：

```yaml
Proxy:
  Routes:
    - Prefix: /api/auth
      Target: http://auth:8081
      StripPrefix: /api
```

---

## 十七、编译与启动

### 17.1 编译 Shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 17.2 编译 Auth

```powershell
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 17.3 Docker 启动 Auth

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build auth
docker logs ojos-auth
```

预期：

```text
Starting server at 0.0.0.0:8081...
```

---

## 十八、验收命令

### 18.1 Health

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8081/health"
```

预期：

```text
code = 0
msg = success
data.status = ok
```

### 18.2 Login 直连

```powershell
$body = @{
  username = "admin"
  password = "123456"
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8081/auth/login" `
  -ContentType "application/json" `
  -Body $body

$res
```

预期：

```text
code = 0
msg = success
data.token 存在
```

### 18.3 Profile 直连

```powershell
$token = $res.data.token

Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8081/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期：

```text
code = 0
msg = success
data.user_id = 1
data.username = admin
```

### 18.4 Gateway 登录

```powershell
$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/login" `
  -ContentType "application/json" `
  -Body $body

$res
```

### 18.5 Gateway Profile

```powershell
$token = $res.data.token

Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

---

## 十九、当前数据库依赖

Auth 依赖以下表：

```text
users
roles
user_roles
```

### 19.1 users

用于存储用户基本信息：

```text
id
username
email
password_hash
created_at
updated_at
```

### 19.2 roles

用于存储角色：

```text
id
name
description
created_at
```

当前基础角色：

```text
super_admin
admin
user
```

### 19.3 user_roles

用于存储用户和角色关系：

```text
user_id
role_id
```

新注册用户默认分配：

```text
user
```

---

## 二十、当前已解决问题记录

### 20.1 PowerShell go mod edit 解析问题

错误命令：

```powershell
go mod edit -require=ojos-shared@v0.0.0
```

在 PowerShell 下可能解析错误。

正确写法：

```powershell
go mod edit '-require=ojos-shared@v0.0.0'
go mod edit '-replace=ojos-shared=../shared'
```

或者直接手改 `go.mod`。

---

### 20.2 goctl 默认模板残留

曾出现：

```text
undefined: types.Request
undefined: types.Response
```

原因：

```text
goctl api new 默认生成的 authlogic.go / authhandler.go 残留
```

解决：

```powershell
Remove-Item internal\logic\authlogic.go -Force
Remove-Item internal\handler\authhandler.go -Force
goctl api go -api auth.api -dir .
```

---

### 20.3 AuthService 参数不匹配

曾出现：

```text
too many arguments in call to AuthService.Login
too many arguments in call to AuthService.Register
```

原因：

```text
AuthService.Login / Register 接收的是 request struct
不是散装 username/password 参数
```

正确调用：

```go
AuthService.Login(ctx, service.LoginRequest{
    Username: req.Username,
    Password: req.Password,
})
```

```go
AuthService.Register(ctx, service.RegisterRequest{
    Username: req.Username,
    Email:    req.Email,
    Password: req.Password,
})
```

---

## 二十一、当前限制

### 21.1 JWT Secret 仍是开发配置

当前：

```yaml
Jwt:
  Secret: ojos-dev-secret-change-me
```

生产环境必须改为强随机密钥，并通过环境变量或密钥管理系统注入。

---

### 21.2 没有刷新 Token

当前只支持登录签发 access token。

未支持：

```text
refresh token
token revoke
token blacklist
多端登录管理
```

---

### 21.3 Gateway 尚未统一鉴权

当前鉴权在 Auth 服务内部完成。

Gateway 只是转发：

```text
/api/auth/*
```

后续可以在 Gateway 层增加 JWT 校验，并向下游透传：

```text
X-User-Id
X-Username
X-Roles
```

---

### 21.4 错误码体系仍需统一

Auth 当前返回：

```json
{
  "code": 40012,
  "msg": "invalid username or password"
}
```

但其他 go-zero 服务可能不是同一格式。

后续需要设计全局错误码规范。

---

### 21.5 权限系统仍然较基础

当前角色有：

```text
super_admin
admin
user
```

但还没有完整 RBAC 权限点系统。

后续可以扩展：

```text
permissions
role_permissions
user_permissions
```

---

## 二十二、后续计划

Auth 后续建议开发顺序：

```text
1. Gateway JWT 鉴权与用户信息透传
2. Auth 增加刷新 Token
3. Auth 增加修改密码
4. Auth 增加登出 / Token 失效
5. Auth 增加权限点表
6. Auth 增加管理员分配角色接口
7. 统一错误码体系
8. 生产级 Secret 管理
```

优先级最高的是：

```text
Gateway JWT 鉴权与用户信息透传
```

因为后续 Judge、Problem、Contest 等服务都需要识别当前用户。

---

## 二十三、当前结论

Auth 当前已经从旧手写服务重构为 go-zero 标准服务。

当前已经完成：

```text
go-zero 结构
shared 接入
JWT 登录
Profile 鉴权
数据库用户与角色读取
Gateway 代理兼容
编译通过
```

当前 Auth 可以作为 OJOS 平台的认证服务继续使用。

下一阶段重点是：

```text
Gateway 统一鉴权
用户信息透传
权限系统扩展
```
