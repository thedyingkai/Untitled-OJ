# OJOS Auth 模块开发文档

## 一、模块定位

`services/auth` 是 OJOS 平台中的认证服务，负责用户身份相关能力。

当前 Auth Service 已经完成 MVP，包括：

```text
健康检查
用户注册
用户登录
JWT 生成
JWT 解析
Profile 查询
默认角色绑定
bcrypt 密码加密
PostgreSQL 持久化
NATS 事件发布
OpenTelemetry + Jaeger 链路追踪
Zap 结构化日志
Docker Compose 部署
```

Auth Service 是一个独立微服务，监听端口：

```text
8081
```

当前可直接访问：

```text
http://localhost:8081
```

后续 Gateway 接入后，外部请求将统一通过 Gateway 转发到 Auth Service。

---

# 二、当前目录结构

当前 Auth 模块目录结构如下：

```text
services/auth/

├── configs/
│   └── config.yaml
│
├── internal/
│   ├── app/
│   │   └── app.go
│   │
│   ├── handler/
│   │   ├── health.go
│   │   ├── login.go
│   │   ├── profile.go
│   │   └── register.go
│   │
│   ├── middleware/
│   │   └── auth.go
│   │
│   ├── model/
│   │   └── user.go
│   │
│   ├── repository/
│   │   └── user_repository.go
│   │
│   ├── router/
│   │   └── router.go
│   │
│   ├── service/
│   │   └── auth_service.go
│   │
│   └── token/
│       └── jwt.go
│
├── Dockerfile
├── go.mod
├── go.sum
└── main.go
```

---

# 三、模块依赖

Auth Service 复用 `services/shared` 中的基础设施能力。

当前依赖：

```text
ojos-shared/config
ojos-shared/logger
ojos-shared/database
ojos-shared/tracing
ojos-shared/events
ojos-shared/middleware
ojos-shared/response
```

第三方依赖包括：

```text
go-zero/rest
pgxpool
bcrypt
golang-jwt/jwt/v5
OpenTelemetry
Zap
NATS
Viper
```

其中：

```text
bcrypt
```

用于密码加密与验证。

```text
golang-jwt/jwt/v5
```

用于 JWT 生成与解析。

---

# 四、配置文件

Auth 配置文件位于：

```text
services/auth/configs/config.yaml
```

当前配置示例：

```yaml
service:
  name: auth-service
  port: 8081

database:
  url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

jaeger:
  endpoint: ojos-jaeger:4317

nats:
  url: nats://ojos-nats:4222

jwt:
  secret: ojos-dev-secret-change-me
  expire_hours: 24
```

说明：

```text
service.name
```

用于日志服务名和 Jaeger service name。

```text
service.port
```

用于 Auth HTTP 服务监听端口。

```text
database.url
```

用于连接 PostgreSQL。

```text
jaeger.endpoint
```

用于 OpenTelemetry OTLP gRPC 上报到 Jaeger。

```text
nats.url
```

用于连接 NATS EventBus。

```text
jwt.secret
```

用于 JWT 签名密钥。

```text
jwt.expire_hours
```

用于设置 JWT 过期时间。

---

# 五、数据库依赖

Auth Service 当前依赖三张数据库表：

```text
users
roles
user_roles
```

## 5.1 users 表

用于存储用户基础信息。

核心字段：

```text
id
username
email
password_hash
created_at
updated_at
```

当前注册接口会写入：

```text
username
email
password_hash
```

其中 `password_hash` 是 bcrypt 加密后的密码，不保存明文密码。

---

## 5.2 roles 表

用于存储角色信息。

当前初始化角色包括：

```text
super_admin
admin
user
```

当前注册用户默认绑定：

```text
user
```

---

## 5.3 user_roles 表

用于维护用户与角色的多对多关系。

注册成功后，会自动插入：

```text
user_id -> user role_id
```

当前已验证：

```text
admin -> user
```

---

# 六、App 初始化流程

Auth Service 的核心初始化逻辑位于：

```text
services/auth/internal/app/app.go
```

`App` 结构体保存服务运行依赖：

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
创建 auth.startup span
    ↓
返回 App
```

关闭流程：

```text
关闭 PostgreSQL 连接池
关闭 NATS EventBus
Shutdown TracerProvider
Sync Logger
```

---

# 七、main.go 启动流程

Auth Service 入口位于：

```text
services/auth/main.go
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
输出 auth listening 日志
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

作为 HTTP Server 框架。

---

# 八、Router 路由层

Auth 路由集中注册在：

```text
services/auth/internal/router/router.go
```

当前已注册接口：

```text
GET  /health
POST /auth/register
POST /auth/login
GET  /auth/profile
```

其中：

```text
/auth/profile
```

需要 JWT 鉴权。

---

# 九、接口文档

## 9.1 Health

### 请求

```http
GET /health
```

### 响应

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

### 说明

该接口用于检查 Auth Service 是否正常运行。

同时会发布 NATS 事件：

```text
auth.health.checked
```

---

## 9.2 Register

### 请求

```http
POST /auth/register
Content-Type: application/json
```

### 请求体

```json
{
  "username": "admin",
  "email": "admin@example.com",
  "password": "123456"
}
```

### 成功响应

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

### 业务流程

```text
读取 JSON 请求体
    ↓
校验 username / password
    ↓
bcrypt 生成 password_hash
    ↓
插入 users 表
    ↓
查询默认 user 角色
    ↓
插入 user_roles 表
    ↓
发布 user.registered 事件
    ↓
返回 user_id
```

### 当前校验规则

```text
username 不能为空
username 长度为 3 到 32
password 长度至少为 6
email 可以为空
```

### 错误码

```text
40001 invalid json body
40002 invalid input
40003 user already exists
50001 internal server error
```

---

## 9.3 Login

### 请求

```http
POST /auth/login
Content-Type: application/json
```

### 请求体

```json
{
  "username": "admin",
  "password": "123456"
}
```

### 成功响应

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "token": "JWT_TOKEN",
    "user_id": 1,
    "username": "admin",
    "roles": ["user"]
  }
}
```

### 业务流程

```text
读取 JSON 请求体
    ↓
根据 username 查询用户
    ↓
bcrypt 校验密码
    ↓
查询用户角色
    ↓
生成 JWT
    ↓
发布 user.login 事件
    ↓
返回 token / user_id / username / roles
```

### 错误码

```text
40011 invalid json body
40012 invalid username or password
50011 internal server error
```

### 当前已验证行为

正确密码：

```text
admin / 123456
```

返回 JWT。

错误密码：

```text
admin / wrong-password
```

返回：

```json
{
  "code": 40012,
  "msg": "invalid username or password"
}
```

---

## 9.4 Profile

### 请求

```http
GET /auth/profile
Authorization: Bearer <JWT_TOKEN>
```

### 成功响应

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

### 业务流程

```text
读取 Authorization Header
    ↓
检查 Bearer Token 格式
    ↓
解析 JWT
    ↓
将 Claims 写入 context
    ↓
Profile Handler 从 context 获取用户信息
    ↓
返回 user_id / username / roles
```

### 错误码

```text
40101 missing authorization header
40102 invalid authorization header
40103 empty token
40104 invalid or expired token
40105 unauthorized
```

### 当前已验证行为

携带合法 token：

```text
GET /auth/profile
```

返回用户信息。

不携带 token：

```json
{
  "code": 40101,
  "msg": "missing authorization header"
}
```

---

# 十、JWT 设计

JWT 工具位于：

```text
services/auth/internal/token/jwt.go
```

## 10.1 Claims 结构

```go
type Claims struct {
    UserID   int64    `json:"user_id"`
    Username string   `json:"username"`
    Roles    []string `json:"roles"`
    jwt.RegisteredClaims
}
```

包含：

```text
user_id
username
roles
iss
sub
iat
exp
```

## 10.2 Generate

用于登录成功后生成 token。

```go
Generate(secret, expireHours, userID, username, roles)
```

## 10.3 Parse

用于 Profile 或后续鉴权中间件解析 token。

```go
Parse(secret, tokenString)
```

当前签名算法：

```text
HS256
```

---

# 十一、Auth Middleware

JWT 鉴权中间件位于：

```text
services/auth/internal/middleware/auth.go
```

当前职责：

```text
读取 Authorization Header
检查 Bearer 格式
解析 JWT
校验 token
将 Claims 写入 context
调用后续 handler
```

Profile Handler 通过：

```go
ClaimsFromContext(ctx)
```

获取用户信息。

---

# 十二、Repository 层

Repository 位于：

```text
services/auth/internal/repository/user_repository.go
```

当前提供：

```text
CreateUserWithDefaultRole
GetByUsername
GetRolesByUserID
```

## 12.1 CreateUserWithDefaultRole

用于注册用户。

内部使用数据库事务：

```text
BEGIN
    INSERT INTO users
    SELECT default user role
    INSERT INTO user_roles
COMMIT
```

如果 username 或 email 唯一约束冲突，返回：

```text
ErrUserExists
```

## 12.2 GetByUsername

用于登录查询用户密码哈希。

返回：

```text
user_id
password_hash
```

## 12.3 GetRolesByUserID

用于登录时查询用户角色。

返回：

```text
[]string
```

例如：

```text
["user"]
```

---

# 十三、Service 层

Service 位于：

```text
services/auth/internal/service/auth_service.go
```

当前提供：

```text
Register
Login
```

## 13.1 Register

负责注册业务逻辑：

```text
参数清洗
参数校验
bcrypt 加密
调用 repository 写库
发布 user.registered 事件
返回注册结果
```

## 13.2 Login

负责登录业务逻辑：

```text
参数清洗
查询用户
bcrypt 校验密码
查询角色
生成 JWT
发布 user.login 事件
返回登录结果
```

---

# 十四、事件设计

Auth 当前发布以下事件：

```text
auth.health.checked
user.registered
user.login
```

事件通过：

```text
NATS
```

发送。

事件基础结构由 shared/events 统一定义。

后续可以扩展：

```text
user.password_changed
user.role_changed
user.disabled
user.deleted
```

---

# 十五、日志与链路追踪

Auth 已接入 shared middleware，因此每个 HTTP 请求都会输出结构化日志。

示例：

```json
{
  "level": "info",
  "msg": "http request",
  "service": "auth-service",
  "trace_id": "...",
  "span_id": "...",
  "method": "POST",
  "path": "/auth/login",
  "status": 200,
  "duration": 0.050976574
}
```

当前已验证接口：

```text
POST /auth/login
GET /auth/profile
```

均能输出：

```text
trace_id
span_id
method
path
status
duration
```

Jaeger 中应出现：

```text
auth-service: auth.startup
auth-service: POST /auth/login
auth-service: GET /auth/profile
```

---

# 十六、Dockerfile

Auth Dockerfile 位于：

```text
services/auth/Dockerfile
```

核心逻辑：

```text
使用 golang:1.26.3
设置 WORKDIR /app
复制 auth/go.mod auth/go.sum
复制 shared/go.mod shared/go.sum
go mod download
复制 auth 源码
复制 shared 源码
go build -o auth .
CMD ["./auth"]
```

因为 Auth 依赖 shared，所以 Docker build context 必须是：

```text
../../services
```

而不能是：

```text
../../services/auth
```

---

# 十七、Docker Compose 集成

Auth 在 Docker Compose 中配置：

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

当前 Auth 对外暴露：

```text
localhost:8081
```

后续 Gateway 接入后，外部应优先访问 Gateway。

---

# 十八、当前验收结果

## 18.1 容器状态

当前已验证：

```text
ojos-auth Up
compose-gateway-1 Up
ojos-postgres Healthy
ojos-redis Up
ojos-nats Up
ojos-jaeger Up
```

## 18.2 Health

```http
GET http://localhost:8081/health
```

返回：

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

## 18.3 Register

```http
POST http://localhost:8081/auth/register
```

成功返回：

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

## 18.4 Login

```http
POST http://localhost:8081/auth/login
```

成功返回：

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

错误密码返回：

```json
{
  "code": 40012,
  "msg": "invalid username or password"
}
```

## 18.5 Profile

携带 token：

```http
GET http://localhost:8081/auth/profile
Authorization: Bearer <token>
```

返回：

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

不携带 token：

```json
{
  "code": 40101,
  "msg": "missing authorization header"
}
```

## 18.6 数据库验收

当前已验证：

```sql
SELECT id, username, email FROM users;
```

结果：

```text
1 | admin | admin@example.com
```

```sql
SELECT * FROM user_roles;
```

结果：

```text
user_id = 1
role_id = 3
```

关联查询：

```sql
SELECT u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id;
```

结果：

```text
admin | user
```

---

# 十九、已解决问题记录

## 19.1 go get 执行目录错误

曾在 `services/auth` 没有 go.mod 时执行：

```powershell
go get golang.org/x/crypto/bcrypt
```

报错：

```text
go.mod file not found
```

原因是 auth 当时还不是 Go module。

解决方式：

```powershell
go mod init ojos-auth
```

之后再执行：

```powershell
go get ...
```

---

## 19.2 bcrypt 不是 main package

曾执行：

```powershell
go install golang.org/x/crypto/bcrypt@latest
```

报错：

```text
package golang.org/x/crypto/bcrypt is not a main package
```

原因是 bcrypt 是库，不是可执行命令。

正确方式：

```powershell
go get golang.org/x/crypto/bcrypt
```

---

## 19.3 Auth 配置文件找不到

曾出现：

```text
Config File "configs" Not Found in "[/app/auth/configs]"
```

原因是配置文件路径或文件名与 Viper 加载规则不一致。

正确约定：

```text
configs/config.yaml
```

---

## 19.4 PowerShell curl JSON 转义问题

曾使用：

```powershell
curl.exe -X POST http://localhost:8081/auth/register `
  -H "Content-Type: application/json" `
  -d '{"username":"admin","email":"admin@example.com","password":"123456"}'
```

返回：

```json
{
  "code": 40001,
  "msg": "invalid json body"
}
```

原因是 Windows PowerShell 下 JSON 字符串转义不稳定。

推荐使用：

```powershell
$body = @{
  username = "admin"
  email = "admin@example.com"
  password = "123456"
} | ConvertTo-Json -Compress

Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8081/auth/register" `
  -ContentType "application/json" `
  -Body $body
```

---

# 二十、当前完成状态

Auth MVP 当前完成：

```text
Auth Skeleton        ✅
Health               ✅
Register             ✅
Login                ✅
bcrypt password hash ✅
JWT Generate         ✅
JWT Parse            ✅
Profile              ✅
Default role binding ✅
NATS events          ✅
PostgreSQL storage   ✅
Docker Compose       ✅
Logging              ✅
Tracing              ✅
```

---

# 二十一、后续计划

Auth 后续可继续扩展：

```text
刷新 Token
登出 / Token 黑名单
修改密码
重置密码
邮箱验证
用户禁用
角色变更
管理员创建用户
权限校验中间件
RBAC 权限模型
多端登录管理
```

当前 Auth MVP 已经完成，可以进入下一阶段：

```text
Gateway 接入 Auth Service
```

目标：

```text
POST /api/auth/register
POST /api/auth/login
GET  /api/auth/profile
```

由 Gateway 转发到内部：

```text
auth-service:8081
```
