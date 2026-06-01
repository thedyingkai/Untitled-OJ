# OJOS Auth 模块开发文档

## 一、模块定位

`services/auth` 是 OJOS 平台中的认证服务。

它负责处理与用户身份相关的基础能力，包括：

```text id="qbaeu4"
用户注册
用户登录
密码哈希
JWT 签发
JWT 校验辅助
用户 Profile 查询
用户基础角色读取
注册用户默认角色绑定
```

Auth 模块的核心职责是回答：

```text id="qna1gk"
你是谁？
你的用户名是什么？
你的用户 ID 是什么？
你的基础角色有哪些？
你的 token 是否有效？
```

Auth 模块不负责回答：

```text id="r53var"
你能不能创建题目？
你能不能管理比赛？
你能不能查看某个私有题？
你能不能发气球？
你能不能打印代码？
你能不能安装模块？
```

这些属于资源级权限判断，应由：

```text id="mkrl8j"
Permission Core
```

负责。

因此 Auth 与 Permission Core 的边界是：

```text id="cljb18"
Auth 负责身份认证
Permission Core 负责资源授权
Gateway 负责入口鉴权与用户上下文透传
业务服务负责选择具体权限点
```

当前 Auth 是一个独立 go-zero HTTP 服务，监听端口：

```text id="u35q7s"
8081
```

内部服务地址：

```text id="cdf3jn"
http://auth:8081
```

宿主机直连地址：

```text id="bl953l"
http://localhost:8081
```

通过 Gateway 暴露的外部路径为：

```text id="fa8pl7"
/api/auth/*
```

Gateway 会将其转发到 Auth 内部路径：

```text id="t2bm0o"
/auth/*
```

例如：

```text id="fcy4o4"
POST /api/auth/login
    -> POST http://auth:8081/auth/login
```

---

## 二、当前版本状态

当前 Auth 已完成 go-zero 重构。

当前版本可以记为：

```text id="vv3so7"
Auth v0.2+
```

当前已完成：

```text id="p43v1s"
go-zero 标准服务结构
auth.api 接口定义
goctl 生成 handler / logic / types / routes / middleware
健康检查接口
用户注册接口
用户登录接口
用户 Profile 接口
bcrypt 密码加密
bcrypt 密码校验
JWT 签发
JWT 解析辅助
默认 user 角色绑定
PostgreSQL 持久化
shared/database 接入
shared/logger 接入
shared/tracing 接入
shared/middleware 接入
shared/security/jwt 接入
Dockerfile 更新
Docker Compose 部署
go build . 通过
```

当前已经删除或不再使用：

```text id="w45e9z"
旧 internal/app
旧 internal/router
旧手写 main.go
旧 shared/config
旧 shared/response
旧 shared/events
NATS EventBus
用户注册事件发布
用户登录事件发布
```

当前 Auth 不再依赖 NATS。

Auth 不应该再出现：

```text id="ujv59u"
Nats
NatsConfig
nats://ojos-nats:4222
ojos-shared/events
events.NewBusByURL
Bus *events.Bus
```

如果这些内容仍然出现在 Auth 源码或配置中，说明 NATS 清理没有完成。

---

## 三、当前目录结构

当前 Auth 模块目录结构应为：

```text id="drm9mx"
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

说明：

```text id="cmt4m8"
handler / logic / types / routes / middleware 主要由 goctl 生成
repository 是手写的数据访问层
service 是手写的认证业务层
svc 是 go-zero ServiceContext 依赖注入层
token 可以作为 Auth 内部 JWT 适配层
```

已删除或不再使用的旧结构：

```text id="jdaaeu"
services/auth/configs/config.yaml
services/auth/internal/app
services/auth/internal/router
services/auth/internal/model
services/auth/main.go
```

如果项目中仍有这些旧路径，需要确认是否已经废弃。当前 go-zero 标准入口应为：

```text id="e2bjmi"
services/auth/auth.go
```

---

## 四、go-zero 重构说明

Auth 使用 go-zero API 模式。

最初生成方式可以是：

```powershell id="o6w4nm"
cd D:\Untitled-OJ\services

goctl api new auth --module ojos-auth
```

当前实际使用的接口描述文件为：

```text id="s95ezq"
services/auth/auth.api
```

修改 `auth.api` 后重新生成：

```powershell id="rhxvqf"
cd D:\Untitled-OJ\services\auth

goctl api go -api auth.api -dir . --style gozero
```

建议统一使用项目脚本：

```powershell id="avjo67"
cd D:\Untitled-OJ

.\scripts\gen-gozero.ps1 -Service auth
```

或者生成全部 go-zero 服务：

```powershell id="oyg5m5"
.\scripts\gen-gozero.ps1
```

注意：

```text id="v8kglt"
goctl 生成文件属于源码
handler / logic / types / routes 应进入 Git
不要把生成文件当成临时文件忽略
```

如果出现默认模板残留，例如：

```text id="w3xu4y"
internal/logic/authlogic.go
internal/handler/authhandler.go
```

并且其中引用不存在的：

```go id="o2z7bc"
types.Request
types.Response
```

应删除这些残留文件：

```powershell id="rk4xw7"
Remove-Item internal\logic\authlogic.go -Force
Remove-Item internal\handler\authhandler.go -Force
```

这些残留通常来自 `goctl api new` 默认模板，与当前 `auth.api` 不匹配。

---

## 五、auth.api

路径：

```text id="dmrtf4"
services/auth/auth.api
```

当前 Auth API 包含：

```text id="s1wtci"
GET  /health
POST /auth/register
POST /auth/login
GET  /auth/profile
```

其中：

```text id="zzh98f"
/auth/profile
```

需要 AuthMiddleware。

接口分组推荐如下：

```go id="d9licc"
syntax = "v1"

info(
    title: "OJOS Auth API"
    desc: "OJOS authentication service"
    author: "thedyingkai"
    version: "v1"
)

type HealthResp {
    Status string `json:"status"`
}

type RegisterReq {
    Username string `json:"username"`
    Email    string `json:"email"`
    Password string `json:"password"`
}

type RegisterResp {
    UserId   int64  `json:"user_id"`
    Username string `json:"username"`
    Email    string `json:"email"`
}

type LoginReq {
    Username string `json:"username"`
    Password string `json:"password"`
}

type LoginResp {
    Token string `json:"token"`
}

type ProfileResp {
    UserId   int64    `json:"user_id"`
    Username string   `json:"username"`
    Email    string   `json:"email"`
    Roles    []string `json:"roles"`
}

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

说明：

```text id="luvu6a"
/health 不带 /auth 前缀
/register 和 /login 属于 /auth 前缀
/profile 属于 /auth 前缀并需要中间件
```

Gateway 转发时会将：

```text id="ce8iee"
/api/auth/login
```

转成：

```text id="ddsqcz"
/auth/login
```

因此 `auth.api` 中内部路径必须是 `/auth/login`，不要写成 `/api/auth/login`。

---

## 六、配置文件

路径：

```text id="vubcuu"
services/auth/etc/auth.yaml
```

当前推荐配置：

```yaml id="yk32sa"
Name: auth-service
Host: 0.0.0.0
Port: 8081

Database:
  Url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

Jaeger:
  Endpoint: ojos-jaeger:4317

Jwt:
  Secret: ojos-dev-secret-change-me
  ExpireHours: 24
```

字段说明：

| 字段                | 说明                 |
| ----------------- | ------------------ |
| `Name`            | 服务名，用于日志和 tracing  |
| `Host`            | HTTP 监听地址          |
| `Port`            | HTTP 监听端口          |
| `Database.Url`    | PostgreSQL 连接地址    |
| `Jaeger.Endpoint` | OTLP gRPC endpoint |
| `Jwt.Secret`      | JWT 签名密钥           |
| `Jwt.ExpireHours` | JWT 过期时间，单位小时      |

当前配置中不应该再出现：

```yaml id="d1jg9z"
Nats:
  Url: nats://ojos-nats:4222
```

如果仍然存在，应删除。

---

## 七、配置结构

路径：

```text id="l1ofaj"
services/auth/internal/config/config.go
```

当前推荐结构：

```go id="k1u2o5"
package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Jaeger   JaegerConfig
    Jwt      JwtConfig
}

type DatabaseConfig struct {
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

```go id="wyuz2v"
ojos-shared/config
```

也不再使用：

```go id="c1h6tl"
config.Load()
```

因为当前每个 go-zero 服务都应自己定义配置结构。

Auth 配置结构中不应再存在：

```go id="tg8q4m"
Nats NatsConfig
```

也不应再存在：

```go id="r44cbu"
type NatsConfig struct {
    Url string
}
```

---

## 八、ServiceContext

路径：

```text id="cwk94t"
services/auth/internal/svc/servicecontext.go
```

`ServiceContext` 是 go-zero 服务依赖注入中心。

当前推荐结构：

```go id="he73uh"
type ServiceContext struct {
    Config config.Config

    Logger *zap.Logger
    DB     *pgxpool.Pool
    Tracer *sdktrace.TracerProvider

    UserRepo    *repository.UserRepository
    AuthService *service.AuthService

    AuthMiddleware rest.Middleware
}
```

初始化内容：

```text id="yyt76d"
创建 context
初始化 zap logger
初始化 OpenTelemetry tracer
初始化 PostgreSQL pool
初始化 UserRepository
初始化 AuthService
初始化 AuthMiddleware
返回 ServiceContext
```

当前不应再包含：

```go id="ou6rm3"
Bus *events.Bus
```

也不应再初始化：

```go id="l3z7dv"
events.NewBusByURL(c.Nats.Url, c.Name)
```

如果 `ServiceContext` 中仍有 Bus 字段，说明 NATS 清理未完成。

---

### 8.1 初始化流程

推荐流程：

```text id="xlbbo5"
context.Background()
    ↓
logger.New(c.Name)
    ↓
tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
    ↓
database.NewPostgresPoolByURL(ctx, c.Database.Url)
    ↓
repository.NewUserRepository(db)
    ↓
service.NewAuthService(...)
    ↓
middleware.NewAuthMiddleware(...)
    ↓
return ServiceContext
```

如果某些服务暂时直接使用 `pgxpool.New` 而不是 `shared/database`，也可以运行，但推荐统一为 Shared 方式。

---

### 8.2 关闭流程

go-zero 生成的服务默认不一定显式处理 shutdown。

后续建议给 Auth 增加：

```go id="so9u83"
func (s *ServiceContext) Close(ctx context.Context) error
```

关闭：

```text id="g5hbb4"
DB pool
TracerProvider
Logger Sync
```

推荐逻辑：

```go id="7cfe0v"
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

当前没有 Close 不影响 MVP，但后续需要补。

---

## 九、Repository 层

路径：

```text id="o4e5g6"
services/auth/internal/repository
```

推荐文件：

```text id="rwzt8v"
services/auth/internal/repository/user_repository.go
```

Repository 层负责访问数据库，不负责业务判断。

它应该提供：

```text id="m8xmw9"
CreateUser
FindUserByUsername
FindUserByID
FindRolesByUserID
BindDefaultRole
BindRoleByName
```

常见方法：

```go id="d9nr6f"
func (r *UserRepository) CreateUser(
    ctx context.Context,
    username string,
    email string,
    passwordHash string,
) (int64, error)
```

```go id="04ker4"
func (r *UserRepository) FindByUsername(
    ctx context.Context,
    username string,
) (*User, error)
```

```go id="470ehz"
func (r *UserRepository) FindRolesByUserID(
    ctx context.Context,
    userID int64,
) ([]string, error)
```

Repository 不应该直接生成 JWT。

Repository 不应该校验密码。

Repository 不应该关心 HTTP status code。

---

### 9.1 users 表

Auth 依赖：

```text id="fs1ayx"
users
```

核心字段：

```text id="8dmqwn"
id
username
email
password_hash
created_at
updated_at
```

注册时写入：

```text id="x3v0g7"
username
email
password_hash
```

不保存明文密码。

密码必须使用：

```text id="aq9agw"
bcrypt
```

哈希后存储。

---

### 9.2 roles 表

Auth 依赖：

```text id="rmz99d"
roles
```

当前内置基础角色：

```text id="e0u6dr"
super_admin
admin
user
```

Permission Core migration 会扩展 roles 字段：

```text id="pnfdr8"
module_code
description
is_system
created_at
```

Auth 不需要关心资源级角色的具体含义，只需要能读取当前用户的全局角色。

---

### 9.3 user_roles 表

Auth 依赖：

```text id="rdpuwt"
user_roles
```

注册成功后，默认插入：

```text id="f54rbn"
user_id -> user role_id
```

也就是说，新用户默认拥有：

```text id="j37mjl"
user
```

这个角色通过 Permission Core 默认拥有：

```text id="g7n8oc"
judge.submit
submission.view.own
problem.view
contest.view
scoreboard.view
print.request
forum.post
clarification.ask
```

具体权限点由 `role_permissions` 决定，不由 Auth 决定。

---

## 十、Service 层

路径：

```text id="p33rnw"
services/auth/internal/service/auth_service.go
```

AuthService 是认证业务层。

它负责：

```text id="n5z1yb"
注册参数校验
密码哈希
用户唯一性处理
用户创建
默认角色绑定
登录密码校验
JWT 生成
Profile 聚合
```

AuthService 不负责：

```text id="toxtcn"
HTTP 请求解析
HTTP 响应格式
资源级权限判断
Gateway 代理
Redis Stream
Judge 任务
```

---

### 10.1 Register

注册流程：

```text id="xfj8h1"
接收 username / email / password
    ↓
校验 username 非空
    ↓
校验 email 非空
    ↓
校验 password 非空
    ↓
bcrypt 生成 password_hash
    ↓
写入 users
    ↓
绑定默认 user 角色
    ↓
返回 user_id / username / email
```

注册时需要处理：

```text id="glfn4k"
username 重复
email 重复
数据库错误
bcrypt 错误
默认 user 角色不存在
```

当前建议把唯一性错误转为业务错误。后续统一错误响应后，应返回类似：

```json id="rqoh5u"
{
  "code": 40002,
  "msg": "username already exists"
}
```

当前如果还只是返回普通 error，后续统一错误响应阶段再处理。

---

### 10.2 Login

登录流程：

```text id="bnwtqz"
接收 username / password
    ↓
按 username 查询用户
    ↓
不存在则返回认证失败
    ↓
bcrypt.CompareHashAndPassword
    ↓
密码不匹配则返回认证失败
    ↓
读取用户 roles
    ↓
生成 JWT
    ↓
返回 token
```

登录失败不应该区分：

```text id="z1m2rf"
用户不存在
密码错误
```

推荐统一返回：

```text id="dxqcd8"
invalid username or password
```

避免泄露用户名是否存在。

---

### 10.3 Profile

Profile 流程：

```text id="einjsk"
AuthMiddleware 校验 token
    ↓
从 context 读取 user_id
    ↓
查询用户
    ↓
查询用户 roles
    ↓
返回 user_id / username / email / roles
```

Profile 当前用于验证：

```text id="mg5dyq"
JWT 是否有效
Gateway / Auth 鉴权链路是否正常
roles 是否正确写入 token 或数据库
```

---

### 10.4 事件发布已删除

旧 AuthService 可能在注册或登录时发布事件，例如：

```text id="hlo5ep"
user.registered
user.logged_in
auth.health.checked
```

当前已经删除这些逻辑。

原因：

```text id="x6c4r5"
1. NATS 已从当前架构移除
2. Auth 当前不依赖事件链路
3. 事件系统尚未重新设计
4. 可靠任务和普通事件需要区分
```

后续如果要恢复用户事件，应先设计新的事件策略，而不是恢复旧 NATS Bus。

可能的未来方案：

```text id="pkuapi"
Redis Streams for reliable tasks
PostgreSQL outbox for domain events
Module event adapter for optional subscribers
```

但当前不做。

---

## 十一、token 层

路径：

```text id="fzlj8b"
services/auth/internal/token/token.go
```

当前 token 层可以作为 Auth 内部对 shared JWT 的薄封装。

推荐它只做：

```text id="ii1a7x"
调用 ojos-shared/security/jwt.Generate
调用 ojos-shared/security/jwt.Parse
转换 Auth 内部结构
```

不建议在 Auth 内部重复实现一套 JWT Claims。

JWT 核心能力应统一在：

```text id="ezut4p"
services/shared/security/jwt
```

否则 Gateway 和 Auth 的 JWT 解析逻辑容易不一致。

---

### 11.1 JWT Claims

推荐 Claims 包含：

```text id="sf1czf"
user_id
username
roles
iss
sub
iat
exp
```

其中：

```text id="dw3hli"
user_id  是数据库 users.id
username 是用户名
roles    是用户全局角色列表
exp      是过期时间
iat      是签发时间
iss      是签发者
sub      可以使用 user_id 字符串
```

---

### 11.2 Generate 参数顺序

之前开发中曾出现参数顺序不一致导致的编译错误，因此需要固定签名。

推荐：

```go id="zz06lw"
func Generate(
    secret string,
    expireHours int,
    userID int64,
    username string,
    roles []string,
) (string, error)
```

调用时：

```go id="euk4s2"
token, err := token.Generate(
    s.jwtSecret,
    s.jwtExpireHours,
    userID,
    username,
    roles,
)
```

不要来回调整参数顺序。

---

### 11.3 Token 过期时间

当前配置：

```yaml id="fbapzj"
Jwt:
  ExpireHours: 24
```

表示 token 有效期为 24 小时。

后续可以扩展：

```text id="v7pmfy"
refresh token
token revoke
session version
password changed after
device login record
```

当前 MVP 只需要 access token。

---

## 十二、AuthMiddleware

路径：

```text id="sk02hs"
services/auth/internal/middleware/authmiddleware.go
```

AuthMiddleware 用于保护 Auth 自身的 `/auth/profile`。

它负责：

```text id="i0ldgo"
读取 Authorization Header
校验 Bearer 格式
解析 JWT
验证签名
验证过期时间
把用户信息放入 context
```

它不依赖 Gateway。

因为 Auth 的 `/auth/profile` 既可以通过 Gateway 调用，也可以内部直连调试。

---

### 12.1 Authorization Header 格式

请求头：

```text id="syv4wy"
Authorization: Bearer <token>
```

错误情况：

```text id="zhdk4f"
缺少 Authorization
不是 Bearer 格式
token 为空
token 签名错误
token 过期
claims 无效
```

当前错误响应可能还没有统一 JSON 格式。后续统一错误响应时应规范为：

```json id="l44phu"
{
  "code": 40101,
  "msg": "missing authorization header"
}
```

或：

```json id="c7ype0"
{
  "code": 40102,
  "msg": "invalid token"
}
```

---

### 12.2 Auth 与 Gateway 的 JWT 关系

Gateway 也会解析 JWT。

AuthMiddleware 和 Gateway JWT 解析必须使用同一个 shared JWT 实现，避免出现：

```text id="jjkrcu"
Auth 签发 token
Gateway 无法解析
Gateway 解析通过
Auth profile 解析失败
roles 字段格式不一致
过期时间解释不一致
```

因此 JWT 逻辑应统一放在：

```text id="m2ns4i"
services/shared/security/jwt
```

Auth 和 Gateway 都复用它。

---

## 十三、Handler / Logic 层

go-zero 生成结构中：

```text id="ti9buw"
handler 负责 HTTP 层
logic 负责调用业务服务
service 负责核心业务
repository 负责数据库
```

建议保持职责分离。

---

### 13.1 RegisterLogic

路径：

```text id="va8f7p"
services/auth/internal/logic/registerlogic.go
```

职责：

```text id="mo16cm"
接收 RegisterReq
调用 AuthService.Register
转换 RegisterResp
```

不应直接写 SQL。

不应直接 bcrypt。

不应直接生成 JWT。

---

### 13.2 LoginLogic

路径：

```text id="fbbiy3"
services/auth/internal/logic/loginlogic.go
```

职责：

```text id="n4dce2"
接收 LoginReq
调用 AuthService.Login
返回 token
```

如果 AuthService 的 Login 签名是：

```go id="g3m6lc"
Login(ctx context.Context, req service.LoginRequest)
```

logic 层就不应该调用：

```go id="4zdnfo"
Login(ctx, username, password)
```

之前出现过：

```text id="qney46"
too many arguments in call to AuthService.Login
```

原因就是 logic 和 service 签名不一致。

应统一成：

```go id="xzgj7s"
resp, err := l.svcCtx.AuthService.Login(
    l.ctx,
    service.LoginRequest{
        Username: req.Username,
        Password: req.Password,
    },
)
```

---

### 13.3 ProfileLogic

路径：

```text id="wx3h8s"
services/auth/internal/logic/profilelogic.go
```

职责：

```text id="b18hg8"
从 context 获取 user_id
调用 AuthService.Profile
返回用户信息
```

Profile 可以从 AuthMiddleware 写入的上下文中读取 Claims，也可以重新查数据库。

推荐重新查数据库，原因：

```text id="d8dqnd"
确保 email 最新
确保 roles 最新
避免 token 中 roles 陈旧
```

但这会增加一次数据库访问。当前 MVP 两种都可接受。

---

### 13.4 HealthLogic

路径：

```text id="pdicpr"
services/auth/internal/logic/healthlogic.go
```

职责：

```text id="lbgvha"
返回 status=ok
```

当前不要再发布：

```text id="jpid77"
auth.health.checked
```

因为 NATS 已删除。

---

## 十四、接口文档

### 14.1 Health

请求：

```http id="quvz82"
GET /health
```

直连：

```text id="qy3jm2"
http://localhost:8081/health
```

经 Gateway：

```text id="i8i2wc"
http://localhost:8080/api/auth/health
```

是否通过 Gateway 暴露取决于 Gateway 路由配置。

响应示例：

```json id="bysfkm"
{
  "status": "ok"
}
```

说明：

```text id="bs6m0c"
用于检查 Auth 服务是否存活
不需要登录
不发布事件
```

---

### 14.2 Register

请求：

```http id="g22doi"
POST /auth/register
```

Gateway 路径：

```http id="pn66lz"
POST /api/auth/register
```

请求体：

```json id="zth4id"
{
  "username": "permtest",
  "email": "permtest@example.com",
  "password": "123456"
}
```

响应示例：

```json id="vrth99"
{
  "user_id": 2,
  "username": "permtest",
  "email": "permtest@example.com"
}
```

注册成功后数据库变化：

```text id="ey46ua"
users 新增一行
user_roles 新增 user 角色绑定
```

注册不会自动赋予：

```text id="za3hln"
admin
super_admin
problem_owner
contest_manager
```

这些需要后续通过权限管理接口或数据库手动授权。

---

### 14.3 Login

请求：

```http id="uepkdp"
POST /auth/login
```

Gateway 路径：

```http id="i5kkcq"
POST /api/auth/login
```

请求体：

```json id="fpzwsn"
{
  "username": "permtest",
  "password": "123456"
}
```

响应示例：

```json id="s780sn"
{
  "token": "<jwt-token>"
}
```

如果当前接口外层包了一层统一响应，则可能是：

```json id="a5wwhc"
{
  "code": 0,
  "msg": "success",
  "data": {
    "token": "<jwt-token>"
  }
}
```

实际以当前 go-zero types 和 handler 为准。

---

### 14.4 Profile

请求：

```http id="nqz8f4"
GET /auth/profile
```

Gateway 路径：

```http id="nnb01n"
GET /api/auth/profile
```

请求头：

```text id="hz38v0"
Authorization: Bearer <token>
```

响应示例：

```json id="yu7qwa"
{
  "user_id": 2,
  "username": "permtest",
  "email": "permtest@example.com",
  "roles": ["user"]
}
```

Profile 用于验证：

```text id="cvhzjl"
token 是否有效
user_id 是否正确
roles 是否正确
AuthMiddleware 是否正常
Gateway 转发是否正常
```

---

## 十五、与 Gateway 的关系

Auth 服务可以被直接访问：

```text id="y21are"
http://localhost:8081
```

但正常外部访问应通过 Gateway：

```text id="rlha7u"
http://localhost:8080/api/auth/*
```

Gateway 配置示例：

```yaml id="m73rnh"
Proxy:
  Routes:
    - Prefix: /api/auth
      Target: http://auth:8081
      StripPrefix: /api
      AuthMode: optional
```

这里 `StripPrefix: /api` 的含义是：

```text id="h7r9xr"
/api/auth/login
    -> /auth/login
```

`AuthMode: optional` 的含义是：

```text id="s9a7fd"
有 token 则解析
没有 token 也允许通过
```

原因是：

```text id="2g9d62"
register 和 login 不需要登录
profile 自己在 Auth 内部用 AuthMiddleware 保护
```

后续也可以细化 Gateway 路由，让：

```text id="9v08c9"
/api/auth/profile
```

走 `required`，但当前没有必要。

---

## 十六、与 Permission Core 的关系

Auth 不直接做资源级权限判断。

Auth 只负责：

```text id="of72l7"
用户是谁
用户有哪些全局 roles
```

Permission Core 负责：

```text id="7sh23c"
用户在某个 scope 上是否拥有某个 permission
```

注册用户默认拥有：

```text id="1u9vyn"
user
```

这个角色是否能提交代码，不由 Auth 决定，而由 Permission Core 中的数据决定：

```text id="vp0zch"
roles
role_permissions
permissions
```

例如当前：

```text id="zijtwx"
user -> judge.submit
```

因此普通用户可以提交。

如果写入：

```text id="bg40bc"
permission_assignments:
  principal = user:2
  permission = judge.submit
  scope = system:0
  effect = deny
```

则该用户会被禁止提交。

Auth 不需要知道这件事，judge-api 在创建提交时会调用 Permission Core 判断。

---

## 十七、与 Judge API 的关系

Judge API 不再从请求体信任 `user_id`。

旧方式：

```json id="qfxu9m"
{
  "problem_id": 1,
  "user_id": 2,
  "language": "cpp17",
  "code": "..."
}
```

这是不安全的，因为用户可以伪造别人的 `user_id`。

当前方式：

```json id="uwlxjc"
{
  "problem_id": 1,
  "language": "cpp17",
  "code": "..."
}
```

用户身份来自：

```text id="cv0jn5"
Auth 登录得到 JWT
    ↓
Gateway 解析 JWT
    ↓
Gateway 注入 X-User-Id
    ↓
judge-api UserContextMiddleware 读取 X-User-Id
    ↓
submissions.user_id = 当前登录用户 ID
```

Auth 在这条链路中负责签发正确 JWT。

---

## 十八、数据库初始化依赖

Auth 依赖 migration 初始化：

```text id="gslq9w"
000001_init_schema
000003_permission_core
```

`000001_init_schema` 应创建基础表：

```text id="r4ozv0"
users
roles
user_roles
```

`000003_permission_core` 会扩展或补充：

```text id="8lfao1"
roles.module_code
roles.description
roles.is_system
roles.created_at
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

如果 Auth 注册时报错：

```text id="jnol47"
relation "users" does not exist
```

说明 migration 没跑。

如果注册后无法绑定默认角色：

```text id="lvhko9"
role user not found
```

说明 roles 初始化缺失，检查 migration。

---

## 十九、开发与编译

### 19.1 本地编译

```powershell id="kfmke8"
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 19.2 重新生成 go-zero 文件

```powershell id="g0i7fa"
cd D:\Untitled-OJ\services\auth

goctl api go -api auth.api -dir . --style gozero
```

或：

```powershell id="pjb7d9"
cd D:\Untitled-OJ

.\scripts\gen-gozero.ps1 -Service auth
```

### 19.3 Docker 重建

```powershell id="ludsdh"
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build auth
```

### 19.4 查看日志

```powershell id="w1c745"
docker logs ojos-auth --tail 100
```

---

## 二十、验收命令

### 20.1 Health

```powershell id="lhsglu"
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/health"
```

或者直连：

```powershell id="czb8qq"
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8081/health"
```

预期：

```text id="wjlwga"
status = ok
```

---

### 20.2 Register

```powershell id="bxfvio"
$body = @{
  username = "permtest"
  email = "permtest@example.com"
  password = "123456"
} | ConvertTo-Json -Compress

Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/register" `
  -ContentType "application/json" `
  -Body $body
```

如果用户已存在，可以忽略，继续登录测试。

---

### 20.3 Login

```powershell id="nl3mt6"
$body = @{
  username = "permtest"
  password = "123456"
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/login" `
  -ContentType "application/json" `
  -Body $body

$token = $res.data.token
$token
```

如果响应不是 `data.token`，而是直接 `token`，则使用：

```powershell id="nn4p5b"
$token = $res.token
```

以当前实际响应结构为准。

---

### 20.4 Profile

```powershell id="u7po8p"
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期包含：

```text id="gsxwyq"
user_id
username
email
roles
```

---

### 20.5 检查数据库角色

```powershell id="zucwsc"
docker exec -it ojos-postgres psql -U postgres -d ojos
```

```sql id="t5wnuz"
SELECT u.id, u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
WHERE u.username = 'permtest'
ORDER BY r.name;
```

预期：

```text id="mtzjjo"
permtest | user
```

---

## 二十一、常见问题

### 21.1 go build 报找不到 ojos-shared/events

错误：

```text id="narcrh"
package ojos-shared/events is not in std
```

原因：

```text id="4y5y76"
Auth 中仍残留旧 NATS 事件总线 import
```

排查：

```powershell id="gdi3zd"
cd D:\Untitled-OJ\services\auth

Get-ChildItem -Recurse -Include *.go |
  Select-String -Pattern "ojos-shared/events|events.NewBus|NewBusByURL|Bus"
```

解决：

```text id="nnd5gv"
删除 import
删除 ServiceContext.Bus
删除 events.NewBusByURL
删除 AuthService 中的事件发布
```

---

### 21.2 go build 报 Login 参数不匹配

错误类似：

```text id="bngmc4"
too many arguments in call to l.svcCtx.AuthService.Login
```

原因：

```text id="z0qa34"
logic 层调用方式与 AuthService.Login 签名不一致
```

如果 Service 层定义：

```go id="rmnhb6"
Login(ctx context.Context, req LoginRequest)
```

Logic 层应调用：

```go id="t3cyen"
l.svcCtx.AuthService.Login(
    l.ctx,
    service.LoginRequest{
        Username: req.Username,
        Password: req.Password,
    },
)
```

不要调用：

```go id="o4g0tq"
Login(ctx, req.Username, req.Password)
```

---

### 21.3 go build 报 JWT Generate 参数类型不匹配

错误类似：

```text id="xoyulo"
cannot use s.jwtExpireHours as int64
cannot use userID as string
cannot use username as []string
cannot use roles as int
```

原因：

```text id="f7p6k2"
token.Generate 函数签名和调用参数顺序不一致
```

统一为：

```go id="wvupwp"
Generate(secret string, expireHours int, userID int64, username string, roles []string)
```

调用：

```go id="ts8xhg"
token.Generate(s.jwtSecret, s.jwtExpireHours, userID, username, roles)
```

---

### 21.4 注册成功但 profile roles 为空

排查：

```text id="e3iz20"
1. user_roles 是否写入
2. roles 表是否有 user
3. FindRolesByUserID SQL 是否正确
4. Login 生成 token 时是否读取 roles
5. Profile 是否从数据库重新读取 roles
```

SQL：

```sql id="ey47zr"
SELECT u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
WHERE u.username = 'permtest';
```

---

### 21.5 Gateway 登录正常但 profile 401

排查：

```text id="ezaxfi"
1. Authorization Header 是否传了 Bearer
2. Jwt.Secret 是否 auth 和 gateway 一致
3. token 是否过期
4. Gateway AuthMode 是否影响转发
5. AuthMiddleware 是否使用 shared/security/jwt
```

Auth 和 Gateway 必须使用同一个：

```text id="yc1tpa"
Jwt.Secret
```

---

### 21.6 GoLand 提示 replace 本地路径不可移植

Auth 的 `go.mod` 中可能有：

```go id="hfz3w6"
replace ojos-shared => ../shared
```

这是 monorepo 内正常写法。当前可以接受。

不要为了消除警告去删除 replace，否则 auth 找不到本地 shared。

---

### 21.7 go-zero 生成后手写逻辑被覆盖

goctl 通常会生成 handler / logic / types 等文件。对于已经手写过的 logic，需要注意：

```text id="kwdbsg"
重新生成前先 git diff
生成后检查 logic 是否被覆盖
业务逻辑尽量放 service 层
logic 层只做轻薄调用
```

这样即使重新生成 logic，也更容易恢复。

---

## 二十二、安全注意事项

### 22.1 密码

必须使用 bcrypt。

不允许：

```text id="dy2t0e"
明文存储密码
MD5
SHA1
无 salt 哈希
```

当前推荐：

```go id="ph1uod"
bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
```

校验：

```go id="wmt1cl"
bcrypt.CompareHashAndPassword([]byte(hash), []byte(password))
```

---

### 22.2 JWT Secret

开发环境可使用：

```text id="y0xp0j"
ojos-dev-secret-change-me
```

生产必须改为强随机值。

不要提交生产 secret。

后续建议通过：

```text id="97qp3c"
.env
Docker secret
Kubernetes secret
Vault
```

注入。

---

### 22.3 登录错误信息

不要区分：

```text id="ppj466"
用户不存在
密码错误
```

推荐统一：

```text id="ki0mir"
invalid username or password
```

---

### 22.4 Token 过期

当前使用固定过期时间：

```text id="jiyt3v"
ExpireHours
```

后续可以加入：

```text id="k5u3pl"
refresh token
token revoke
session version
forced logout
password changed at
```

---

### 22.5 Header 信任边界

Auth 可以自己解析 JWT。

下游业务服务不应该自己信任客户端传来的：

```text id="m81syz"
X-User-Id
X-Username
X-Roles
```

这些应由 Gateway 注入。

---

## 二十三、后续规划

Auth 后续可以扩展：

```text id="kdwby1"
统一错误响应
Refresh Token
Token Revocation
修改密码
重置密码
邮箱验证
第三方 OAuth 登录
管理员创建用户
批量导入用户
账号禁用
登录日志
设备管理
Session version
更细的安全审计
```

当前优先级最高的不是这些，而是：

```text id="cnadk4"
统一错误响应
Problem Core / Dataset Core
Permission API
```

Auth 当前已经足够支撑：

```text id="ebvrco"
登录
鉴权
用户身份透传
Judge 提交用户绑定
Permission Core 用户主体判断
```

---

## 二十四、当前结论

Auth 当前已经完成 OJOS 平台的基础认证能力。

它已经从旧的：

```text id="khlxvx"
手写 app/router
shared/config
shared/response
shared/events
NATS 事件发布
```

演进为：

```text id="fok80o"
go-zero 标准认证服务
shared JWT
shared database
shared tracing
shared middleware
PostgreSQL 用户持久化
Gateway 统一入口
Permission Core 身份基础
```

Auth 当前的正确定位是：

```text id="zch4df"
稳定身份服务
不做资源权限
不做业务事件
不做比赛逻辑
不做题库逻辑
```

后续所有需要“当前用户是谁”的服务，都应该通过：

```text id="tz34f3"
Gateway JWT 验证
可信用户 Header
authctx.UserContext
```

建立身份链路。

后续所有需要“当前用户能不能做某件事”的服务，都应该通过：

```text id="wficvy"
Permission Core
```

判断资源级权限。
