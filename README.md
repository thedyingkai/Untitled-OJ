# Untitled-OJ / OJOS

> OJOS: Online Judge Operating System
> 一个面向模块化、事件驱动、可扩展架构设计的现代 Online Judge 基础设施平台。

---

## 一、项目定位

Untitled-OJ 当前定位不是一个传统的单体 Online Judge 网站，而是一个：

```text
OJ Operating System（OJOS）
```

也就是：

```text
Online Judge Infrastructure Platform
```

它的目标不是只实现“能提交代码、能返回 AC/WA”的普通 OJ，而是构建一个可以长期演进、可以按模块安装、可以支持多题型、多赛制、多运行器、多权限范围、多比赛运营能力的 OJ 基础设施平台。

OJOS 的核心目标包括：

```text
高性能
分布式
模块化
可观测
可扩展
可配置
可插拔
支持完整资源级权限控制
支持多语言评测
支持多题型评测
支持多赛制比赛
支持模块化安装与禁用
支持比赛运营能力
支持后续通过 PR 增加新模块
```

项目的长期设计理念是：

```text
Everything is a Module
Everything is Capability-Based
Everything is Extensible
Everything is Permission-Controlled
```

也就是说：

```text
认证是模块
权限是模块
题库是模块
判题是模块
赛制是模块
榜单是模块
气球是模块
打印是模块
帖子是模块
Clarification 是模块
Module Registry / Launcher 也是模块
```

但模块化不等于“每个能力都必须一个 Docker 容器”。OJOS 的模块需要分成：

```text
服务级模块
能力级模块
资源级模块
配置级模块
前端级模块
```

服务级模块可以独立 Docker 化，例如：

```text
gateway
auth
judge-api
judge-worker
problem-api
contest-api
scoreboard-api
launcher
```

能力级模块可以嵌入在某个核心服务内部，例如：

```text
contest-rule-acm
contest-rule-oi
contest-rule-ioi
problem-type-traditional
problem-type-interactive
checker-standard
checker-special
```

这种设计可以避免把系统拆成几十个微服务，导致本地开发、日志排查、网络调用、数据库一致性全部变得不可控。

---

## 二、当前总体状态

当前 OJOS 已经完成了从“基础设施空壳”到“可运行 OJ 原型”的第一阶段跨越。

目前已经完成的能力包括：

```text
Docker Compose 本地编排
PostgreSQL 数据库
Redis 基础设施
Jaeger 链路追踪
数据库迁移体系
go-zero 微服务结构
Auth 认证服务
Gateway 统一入口
Shared 公共基础库
完整资源级 Permission Core
Judge API MVP
Rust Judge Worker
Redis Streams Judge Queue
多语言评测配置
Gateway JWT 鉴权
可信用户上下文透传
Judge API 权限检查
Judge Worker PENDING 兜底恢复
Judge Worker 原子抢任务
Redis Stream 消费确认
```

当前可以认为已经完成：

```text
Infrastructure Foundation
Shared v0.3+
Gateway v0.3+
Auth v0.2+
Permission Core v1
Judge API MVP v0.3+
Judge Worker Reliability v0.3
Judge Queue Redis Streams v0.3
```

当前系统已经可以跑通：

```text
用户注册
用户登录
JWT 签发
Gateway 验证 JWT
Gateway 注入可信用户上下文
Judge API 读取可信用户身份
Judge API 检查 judge.submit 权限
Judge API 创建 submission
Judge API 写入 Redis Stream
Judge Worker 通过 Redis Consumer Group 消费任务
Judge Worker 原子抢占 submission
Judge Worker 编译运行代码
Judge Worker 写入 submission_cases
Judge Worker 更新 submissions
Judge Worker XACK Redis Stream 消息
用户查询提交结果
```

当前已经完成的真实验证包括：

```text
permtest 普通用户可以登录
permtest 只有 user 角色
user 角色拥有 judge.submit
permtest 可以正常提交代码
提交记录 user_id 正确写入为 permtest 的用户 ID
提交可以通过 Redis Streams 被 worker 实时消费
worker 可以完成判题并写回 ACCEPTED
Redis XPENDING 为 0
给 permtest 写入 judge.submit deny 后提交被 forbidden 拦截
删除 deny 后提交恢复正常
```

但是当前仍然不是生产级完整 OJ。原因是：

```text
Judge Runner 仍未安全沙箱化
用户程序仍直接运行在 judge-worker 容器内
没有真实内存限制
测试数据仍直接存储在数据库 TEXT 字段中
没有文件化测试数据管理
没有 Special Judge
没有子任务 / 捆绑点
没有交互题
没有通信题
没有提交答案题
没有正式 contest-core
没有正式 scoreboard-core
没有 module-registry / launcher
权限核心已完成，但还没有权限管理 API / UI
```

因此，当前系统定义为：

```text
可运行 OJ 原型 / 基础设施 MVP
```

而不是：

```text
生产级完整 OJ
```

---

## 三、技术栈

| 模块                    | 技术                                 |
| --------------------- | ---------------------------------- |
| Backend API           | Go                                 |
| API Framework         | go-zero                            |
| Judge Worker          | Rust                               |
| Judge Queue           | Redis Streams                      |
| Judge Queue Mode      | Consumer Group / XREADGROUP / XACK |
| Judge Language Config | YAML                               |
| Database              | PostgreSQL                         |
| DB Migration          | golang-migrate                     |
| Go DB Driver          | pgx / pgxpool                      |
| Rust DB Driver        | sqlx                               |
| Cache / Queue         | Redis                              |
| Tracing               | OpenTelemetry                      |
| Trace UI              | Jaeger                             |
| Go Logger             | Zap                                |
| Rust Logger           | tracing                            |
| Deployment            | Docker Compose                     |
| Auth                  | JWT / bcrypt                       |
| Permission            | Resource-level RBAC + ACL          |
| C++ Toolchain         | g++                                |
| C Toolchain           | gcc                                |
| Script Language       | Python 3                           |
| JVM Language          | OpenJDK                            |
| Go Judge Toolchain    | Go                                 |
| Rust Judge Toolchain  | Rust                               |

当前已经从旧的：

```text
NATS Core Pub/Sub
```

迁移为：

```text
Redis Streams Reliable Queue
```

当前 Judge 任务链路不再依赖 NATS。

Redis 在当前系统中承担两类职责：

```text
可靠任务队列
后续缓存基础设施
```

其中 Judge 队列使用：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

---

## 四、当前 Monorepo 结构

当前项目结构建议保持如下：

```text
Untitled-OJ/

├── frontend/
│   ├── public/
│   ├── src/
│   ├── package.json
│   ├── package-lock.json
│   ├── vite.config.ts
│   └── tsconfig*.json
│
├── services/
│   ├── shared/
│   ├── gateway/
│   ├── auth/
│   ├── judge-api/
│   └── judge-worker/
│
├── deploy/
│   ├── compose/
│   │   └── docker-compose.yml
│   ├── migrations/
│   │   ├── 000001_init_schema.up.sql
│   │   ├── 000001_init_schema.down.sql
│   │   ├── 000002_judge_schema.up.sql
│   │   ├── 000002_judge_schema.down.sql
│   │   ├── 000003_permission_core.up.sql
│   │   └── 000003_permission_core.down.sql
│   └── observability/
│
├── docs/
│   ├── index.md
│   ├── architecture_overview.md
│   ├── shared_module.md
│   ├── auth_module.md
│   ├── gateway_module.md
│   ├── permission_core_module.md
│   ├── judge_module.md
│   ├── judge_worker_module.md
│   └── development_workflow.md
│
├── proto/
│
├── scripts/
│   └── gen-gozero.ps1
│
├── README.md
└── .gitignore
```

当前核心服务为：

```text
services/shared
services/gateway
services/auth
services/judge-api
services/judge-worker
```

其中：

```text
shared
```

不是独立 HTTP 服务，而是 Go 微服务公共基础库。

```text
permission core
```

当前位于：

```text
services/shared/security/permission
```

它不是单独的 HTTP 服务，而是被各个业务服务调用的权限核心库。

---

## 五、当前核心模块职责

### 5.1 shared

`services/shared` 是 Go 微服务公共基础库。

它负责提供：

```text
PostgreSQL 连接池初始化
Zap logger 初始化
OpenTelemetry tracing 初始化
go-zero Recovery middleware
go-zero Logging middleware
JWT 生成与解析
可信用户上下文解析
完整资源级权限检查
角色绑定
直接授权 / 拒绝
资源继承关系维护
权限点注册
资源类型注册
```

Shared 不负责：

```text
业务配置加载
业务逻辑
HTTP 路由
具体服务启动
题库逻辑
比赛逻辑
判题逻辑
```

Shared 当前已经删除旧的：

```text
shared/config
shared/response
shared/events
shared/events/nats.go
```

也就是说，Shared 当前不再提供 NATS EventBus。

---

### 5.2 gateway

`services/gateway` 是统一 HTTP 入口。

它负责：

```text
监听 8080
提供 /health
通过配置代理 /api/auth
通过配置代理 /api/judge
JWT 验证
AuthMode 判断
清理客户端伪造的可信用户 Header
注入可信用户 Header
转发 trace context
反向代理到内部服务
```

Gateway 不负责具体业务权限判断。

例如：

```text
POST /api/judge/submissions
```

Gateway 只负责判断请求是否已登录，并将用户身份透传给 judge-api。至于该用户是否拥有：

```text
judge.submit @ system:0
```

由 judge-api 调用 Permission Core 判断。

---

### 5.3 auth

`services/auth` 是认证服务。

它负责：

```text
用户注册
用户登录
密码 bcrypt 哈希
JWT 签发
JWT 解析
Profile 查询
用户角色查询
注册用户时绑定默认 user 角色
```

Auth 不负责资源级权限判断。

Auth 的边界是：

```text
你是谁
你的基础角色是什么
你的 token 是否有效
```

资源级权限由 Permission Core 负责。

---

### 5.4 permission core

Permission Core 是完整资源级权限核心。

它的统一判断模型为：

```text
Can(principal, permission, scope)
```

例如：

```text
Can(user:1, "judge.submit", system:0)
Can(user:2, "problem.edit", problem:7)
Can(user:3, "contest.manage", contest:5)
Can(user:4, "balloon.manage", contest:5)
Can(user:5, "module.install", system:0)
```

Permission Core 支持：

```text
principal_type / principal_id
scope_type / scope_id
system:0
type:0
resource_edges 资源继承
role_permissions
role_bindings
permission_assignments allow / deny
super_admin 最高权限
全局 user_roles
资源级角色绑定
```

当前已经接入的真实权限检查是：

```text
POST /judge/submissions
    -> judge.submit @ system:0
```

---

### 5.5 judge-api

`services/judge-api` 是 Judge 的 HTTP API 层。

当前负责：

```text
创建题目 MVP
添加测试点 MVP
创建提交
查询提交
查询测试点结果
读取 Gateway 注入用户上下文
检查 judge.submit 权限
写入 submissions
向 Redis Stream 投递判题任务
```

当前 judge-api 仍然保留了早期 MVP 接口：

```text
POST /judge/problems
POST /judge/test-cases
```

这些接口后续应该迁移到：

```text
problem-api
```

迁移完成后，judge-api 应只负责：

```text
submissions
submission_cases
judge task
rejudge task
```

---

### 5.6 judge-worker

`services/judge-worker` 是 Rust 判题执行器。

它负责：

```text
连接 PostgreSQL
连接 Redis
确保 Redis Consumer Group 存在
启动时扫描 PENDING submissions
定时扫描 PENDING submissions
通过 XREADGROUP 消费 Redis Stream
解析 submission_id
执行 try_claim_submission
编译用户代码
运行测试点
比较输出
写入 submission_cases
更新 submissions
XACK Redis Stream 消息
```

当前 Judge Worker 使用：

```text
Redis Streams
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
```

实现可靠判题任务处理。

---

## 六、基础设施

### 6.1 Docker Compose

当前使用 Docker Compose 编排本地开发环境。

当前基础设施包括：

```text
PostgreSQL
Redis
Jaeger
Gateway
Auth
Judge API
Judge Worker
```

当前不再需要：

```text
NATS
```

启动命令：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build
```

查看运行状态：

```powershell
docker ps
```

查看日志：

```powershell
docker logs ojos-gateway
docker logs ojos-auth
docker logs ojos-judge-api
docker logs ojos-judge-worker
docker logs ojos-redis
docker logs ojos-postgres
```

重建单个服务：

```powershell
docker compose up -d --build gateway
docker compose up -d --build auth
docker compose up -d --build judge-api
docker compose up -d --build judge-worker
```

清理已删除服务：

```powershell
docker compose down --remove-orphans
docker compose up -d --build
```

用于确认 NATS 已经彻底移除：

```powershell
docker ps --filter "name=nats"
docker compose config | Select-String -Pattern "nats|4222"
```

预期应无输出。

---

### 6.2 PostgreSQL

当前数据库名：

```text
ojos
```

PostgreSQL 用于存储：

```text
用户
角色
用户角色
完整资源级权限
题目 MVP 数据
测试点 MVP 数据
提交记录
测试点评测结果
数据库迁移状态
```

当前核心表：

```text
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

problems
test_cases
submissions
submission_cases

schema_migrations
```

进入数据库：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

查看表：

```sql
\dt
```

查看迁移状态：

```sql
SELECT * FROM schema_migrations;
```

查看用户和角色：

```sql
SELECT u.id, u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
ORDER BY u.id, r.name;
```

查看权限点：

```sql
SELECT code, module_code, name
FROM permissions
ORDER BY code;
```

---

### 6.3 Migration

当前使用：

```text
golang-migrate
```

当前 migration 文件：

```text
000001_init_schema
000002_judge_schema
000003_permission_core
```

执行迁移：

```powershell
cd D:\Untitled-OJ

migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

回滚一步：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  down 1
```

创建新的迁移：

```powershell
migrate create `
  -ext sql `
  -dir deploy/migrations `
  -seq problem_core
```

当前迁移原则：

```text
已经上线或被多人使用的 migration 不要随意改历史文件
新增结构应通过新的 migration 引入
down 文件应谨慎，避免误删核心数据
权限相关表的 down 可以删除权限核心表，但生产环境不应随意执行
```

---

### 6.4 Redis

Redis 当前用于：

```text
Judge 可靠任务队列
后续缓存基础设施
```

Judge 队列使用 Redis Streams。

当前 Stream：

```text
ojos:judge:submissions
```

当前 Consumer Group：

```text
judge-workers
```

当前消息字段：

```text
type
producer
submission_id
created_at
```

示例消息：

```text
type          submission.created
producer      judge-api-service
submission_id 16
created_at    2026-05-31T23:39:20Z
```

查看 Stream：

```powershell
docker exec -it ojos-redis redis-cli XINFO STREAM ojos:judge:submissions
```

查看 Consumer Group：

```powershell
docker exec -it ojos-redis redis-cli XINFO GROUPS ojos:judge:submissions
```

查看 pending：

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

查看历史消息：

```powershell
docker exec -it ojos-redis redis-cli XRANGE ojos:judge:submissions - +
```

说明：

```text
XACK 只会从 consumer group pending list 中确认消息
不会从 stream 中删除历史消息
```

后续可以加入：

```text
XTRIM ojos:judge:submissions MAXLEN ~ 10000
```

用于控制 Stream 长度。

---

### 6.5 Jaeger / OpenTelemetry

当前使用：

```text
OpenTelemetry
Jaeger
OTLP gRPC
```

Jaeger UI：

```text
http://localhost:16686
```

当前已接入：

```text
Gateway tracing
Auth tracing
Judge API tracing
Shared tracing 初始化
HTTP middleware trace_id / span_id 注入日志
```

当前 tracing 仍有待完善：

```text
采样率配置
超时控制
失败降级
BatchSpanProcessor
跨服务 trace 串联验收
Redis queue trace propagation
Judge worker trace span
```

当前 GoLand 可能提示：

```text
go.opentelemetry.io/otel/exporters/zipkin 已弃用
```

这是因为 go-zero 当前版本的 trace 包仍可能间接依赖 zipkin exporter。只要业务代码没有直接使用 zipkin，并且 go build 可以通过，当前可暂时接受。后续应通过升级 go-zero 或替换 trace 初始化策略解决。

---

## 七、数据库模型总览

### 7.1 用户认证相关表

```text
users
roles
user_roles
```

`users` 存储：

```text
id
username
email
password_hash
created_at
updated_at
```

`roles` 存储：

```text
id
name
module_code
description
is_system
created_at
```

`user_roles` 存储：

```text
user_id
role_id
```

`user_roles` 当前定义为：

```text
用户的系统级全局角色
```

例如：

```text
permtest -> user
admin    -> user
admin    -> super_admin
```

---

### 7.2 权限核心表

```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

`resource_types` 用于注册资源类型：

```text
system
module
problem
contest
group
team
submission
post
clarification
balloon
print
```

`permissions` 用于注册权限点：

```text
judge.submit
problem.create
problem.edit
problem.manage.data
contest.manage
scoreboard.freeze
balloon.manage
print.operate
module.install
```

`role_permissions` 表示：

```text
某个角色拥有哪些权限点
```

`role_bindings` 表示：

```text
某个主体在某个资源作用域上拥有某个角色
```

`permission_assignments` 表示：

```text
直接 allow / deny 某个权限
```

`resource_edges` 表示：

```text
资源之间的包含 / 从属关系
```

例如：

```text
contest:3 -> problem:7
contest:3 -> submission:100
group:1   -> contest:3
```

`permission_audit_logs` 用于记录权限变更审计。

---

### 7.3 Judge MVP 表

```text
problems
test_cases
submissions
submission_cases
```

`problems` 当前是 MVP 题目表，存储：

```text
id
title
time_limit_ms
memory_limit_mb
created_at
updated_at
```

`test_cases` 当前是 MVP 测试点表，存储：

```text
id
problem_id
input
output
score
created_at
```

当前输入输出直接存在数据库中，后续要迁移到文件化测试数据。

`submissions` 存储：

```text
id
problem_id
user_id
language
code
status
score
time_ms
memory_kb
message
created_at
updated_at
```

`submission_cases` 存储：

```text
id
submission_id
test_case_id
status
time_ms
memory_kb
message
created_at
```

后续 Problem Core 正规化后，题目和测试数据管理将从 judge-api 移动到 problem-api。

---

## 八、Shared 公共模块

路径：

```text
services/shared
```

Shared 当前是 Go 微服务公共基础库，不是服务，不监听端口，不单独 Docker 部署。

当前目录建议为：

```text
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
│   ├── jwt/
│   └── permission/
│
├── tracing/
│   └── tracing.go
│
├── go.mod
└── go.sum
```

当前不应再存在：

```text
services/shared/config
services/shared/response
services/shared/events
```

Shared 的职责边界：

```text
提供基础设施能力
不提供业务逻辑
不加载各服务配置
不持有服务生命周期
不依赖具体服务
```

Shared 提供的主要能力：

```text
database.NewPostgresPoolByURL
logger.New
logger.WithTrace
tracing.InitOTLP
middleware.RecoveryMiddleware
middleware.LoggingMiddleware
security/jwt
security/authctx
security/permission
```

各 Go 服务应自己定义：

```text
internal/config/config.go
etc/*.yaml
ServiceContext
```

然后将必要参数传给 Shared。

---

## 九、Gateway 模块

路径：

```text
services/gateway
```

Gateway 是统一 HTTP 入口。

当前监听：

```text
0.0.0.0:8080
```

当前对外入口：

```text
http://localhost:8080
```

当前自身接口：

```http
GET /health
```

当前代理接口：

```text
/api/auth/*
/api/judge/*
```

Gateway 当前能力：

```text
go-zero 标准服务结构
配置驱动代理
ReverseProxy Rewrite
AuthMode
JWT 验证
可信用户上下文 Header 注入
Header 清理
trace context 传播
日志中间件
panic recovery
```

当前配置示例：

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

Gateway 不应再包含：

```text
Nats
NatsConfig
EventBus
events.NewBusByURL
nats://ojos-nats:4222
```

Gateway 对权限的边界：

```text
Gateway 负责认证
业务服务负责授权
```

也就是说：

```text
Gateway 判断请求有没有合法 JWT
judge-api 判断是否拥有 judge.submit
problem-api 判断是否拥有 problem.create / problem.edit
contest-api 判断是否拥有 contest.manage
launcher 判断是否拥有 module.install
```

---

## 十、Auth 模块

路径：

```text
services/auth
```

Auth 当前监听：

```text
0.0.0.0:8081
```

Gateway 暴露：

```text
/api/auth/*
```

Auth 内部路径：

```text
/auth/*
```

当前接口：

```http
GET  /health
POST /auth/register
POST /auth/login
GET  /auth/profile
```

Auth 当前能力：

```text
go-zero 标准结构
auth.api
用户注册
用户登录
bcrypt 密码哈希
JWT 签发
JWT 解析
Profile 查询
默认 user 角色绑定
PostgreSQL 持久化
OpenTelemetry tracing
Zap logging
Docker Compose 部署
```

Auth 当前不应再包含：

```text
Nats
NatsConfig
EventBus
shared/events
user.registered event publish
user.login event publish
```

Auth 当前配置示例：

```yaml
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

Auth 的职责边界：

```text
负责用户身份
不负责资源级权限
```

注册用户时默认绑定：

```text
user
```

如果需要超级管理员，可通过数据库插入：

```sql
INSERT INTO user_roles(user_id, role_id)
SELECT u.id, r.id
FROM users u
JOIN roles r ON r.name = 'super_admin'
WHERE u.username = 'admin'
ON CONFLICT DO NOTHING;
```

---

## 十一、Permission Core 模块

路径：

```text
services/shared/security/permission
```

Permission Core 是 OJOS 的完整资源级权限核心。

统一判断形式：

```text
HasUserPermission(user_id, permission_code, scope)
```

例如：

```text
HasUserPermission(2, "judge.submit", system:0)
HasUserPermission(3, "problem.edit", problem:7)
HasUserPermission(4, "contest.manage", contest:5)
```

主要类型：

```go
type Principal struct {
    Type string
    ID   int64
}

type Scope struct {
    Type string
    ID   int64
}
```

主要函数：

```text
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

权限判断顺序：

```text
1. super_admin 直接允许
2. 收集当前 scope、父级 scope、type:0、system:0
3. 检查直接 deny
4. 检查直接 allow
5. 检查全局 user_roles
6. 检查资源级 role_bindings
7. 默认拒绝
```

当前已验证：

```text
普通 user 角色拥有 judge.submit
permtest 可以提交
给 permtest 写入 deny judge.submit @ system:0 后提交失败
删除 deny 后提交恢复
```

当前 Permission Core 的目标是：

```text
后续新增模块不改权限核心表
后续新增资源类型只注册 resource_type
后续新增权限点只注册 permission
后续新增角色只注册 role 和 role_permissions
后续新增资源关系只写 resource_edges
```

---

## 十二、Judge API 模块

路径：

```text
services/judge-api
```

Judge API 当前监听：

```text
0.0.0.0:8082
```

Gateway 暴露：

```text
/api/judge/*
```

内部路径：

```text
/judge/*
```

当前接口：

```http
POST /judge/problems
POST /judge/test-cases
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
```

说明：

```text
POST /judge/problems
POST /judge/test-cases
```

是早期 MVP 接口，后续应迁移到 `problem-api`。

Judge API 创建提交时的流程：

```text
读取 Gateway 注入的用户上下文
检查用户是否登录
检查 judge.submit @ system:0
检查 problem_id
检查 language
检查 code
写入 submissions(status=PENDING)
写入 Redis Stream ojos:judge:submissions
返回 submission_id 和 PENDING
```

Redis XADD 消息字段：

```text
type          submission.created
producer      judge-api-service
submission_id <id>
created_at    <utc timestamp>
```

当前 judge-api 不应再包含：

```text
Nats
NatsConfig
nats.Connect
svcCtx.Nats
Nats.Publish
```

当前 judge-api 依赖：

```text
PostgreSQL
Redis
Gateway 用户上下文
Permission Core
```

---

## 十三、Judge Worker 模块

路径：

```text
services/judge-worker
```

Judge Worker 使用 Rust 实现。

它不是 HTTP 服务，而是后台任务进程。

当前职责：

```text
连接 PostgreSQL
连接 Redis
加载 languages.yaml
确保 Redis Consumer Group 存在
启动扫描 PENDING
定时扫描 PENDING
XREADGROUP 消费 Redis Stream
解析 submission_id
try_claim_submission
编译用户代码
运行测试点
比较标准输出
写入 submission_cases
更新 submissions
XACK Redis Stream 消息
```

当前环境变量：

```text
REDIS_URL
DATABASE_URL
LANGUAGES_CONFIG
JUDGE_WORKER_ID
```

默认值示例：

```text
REDIS_URL=redis://ojos-redis:6379/0
DATABASE_URL=postgres://postgres:password@postgres:5432/ojos?sslmode=disable
LANGUAGES_CONFIG=config/languages.yaml
```

当前队列：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

Judge Worker 当前可靠性模型：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
+
Redis XACK
```

核心抢任务 SQL：

```sql
UPDATE submissions
SET status = 'RUNNING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

含义：

```text
只有 PENDING 状态的 submission 才能被当前 worker 抢到
抢不到就跳过
防止重复判题
```

当前已验证：

```text
Redis Stream 实时消息可以被 worker 收到
submission 可以被 claimed
判题可以完成
消息可以 XACK
XPENDING 为 0
历史 PENDING 可以被启动扫描恢复
已经判完的任务再次收到 Stream 消息会被 skip 并 ACK
```

当前限制：

```text
没有独立安全沙箱
没有真实 memory limit
没有网络隔离
没有进程数限制
没有文件系统隔离
没有 Special Judge
没有交互题 runner
没有通信题 runner
没有提交答案题 runner
```

---

## 十四、语言配置

Judge Worker 使用：

```text
services/judge-worker/config/languages.yaml
```

该文件定义每种语言的：

```text
source_file
exe_file
compile.enabled
compile.command
compile.args
compile.timeout_ms
run.command
run.args
```

示例：

```yaml
languages:
  cpp17:
    source_file: main.cpp
    exe_file: main
    compile:
      enabled: true
      command: g++
      args:
        - "-std=c++17"
        - "-O2"
        - "-pipe"
        - "{source}"
        - "-o"
        - "{exe}"
      timeout_ms: 10000
    run:
      command: "{exe}"
      args: []
```

支持占位符：

```text
{source}
{exe}
{workdir}
```

当前设计原则：

```text
一个 judge-worker 可以支持多语言
语言命令不应硬编码在 Rust 代码里
新增语言优先修改 languages.yaml
后续可以把语言包做成独立模块
```

---

## 十五、当前 API 验收命令

### 15.1 Gateway Health

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

### 15.2 注册用户

```powershell
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

---

### 15.3 登录

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

$token = $res.data.token
```

---

### 15.4 查询 Profile

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

---

### 15.5 提交代码

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

---

### 15.6 查询提交结果

```powershell
Start-Sleep -Seconds 2

Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/$($res.submission_id)" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期：

```text
status = ACCEPTED
score = 100
user_id = 当前用户 ID
```

---

### 15.7 查询 Redis Pending

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

预期：

```text
0
```

---

## 十六、权限验收命令

### 16.1 查看用户角色

```sql
SELECT u.id, u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
WHERE u.username = 'permtest'
ORDER BY r.name;
```

预期：

```text
permtest | user
```

### 16.2 写入 deny

```sql
INSERT INTO permission_assignments(
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    reason
)
SELECT
    'user',
    u.id,
    'judge.submit',
    'system',
    0,
    'deny',
    'test deny judge.submit'
FROM users u
WHERE u.username = 'permtest'
ON CONFLICT(principal_type, principal_id, permission_code, scope_type, scope_id)
DO UPDATE SET
    effect = EXCLUDED.effect,
    reason = EXCLUDED.reason;
```

此时 `permtest` 再提交应被拒绝。

### 16.3 删除 deny

```sql
DELETE FROM permission_assignments
WHERE principal_type = 'user'
  AND principal_id = (SELECT id FROM users WHERE username = 'permtest')
  AND permission_code = 'judge.submit'
  AND scope_type = 'system'
  AND scope_id = 0;
```

删除后提交应恢复。

---

## 十七、本地编译

### 17.1 Shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 17.2 Auth

```powershell
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 17.3 Gateway

```powershell
cd D:\Untitled-OJ\services\gateway

go mod tidy
go build .
```

### 17.4 Judge API

```powershell
cd D:\Untitled-OJ\services\judge-api

go mod tidy
go build .
```

### 17.5 Judge Worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

### 17.6 Frontend

```powershell
cd D:\Untitled-OJ\frontend

npm install
npm run dev
npm run build
```

---

## 十八、go-zero 生成流程

Auth、Gateway、Judge API 都是 go-zero API 服务。

`.api` 文件修改后，需要重新生成：

```powershell
cd D:\Untitled-OJ\services\auth
goctl api go -api auth.api -dir . --style gozero

cd D:\Untitled-OJ\services\gateway
goctl api go -api gateway.api -dir . --style gozero

cd D:\Untitled-OJ\services\judge-api
goctl api go -api judgeapi.api -dir . --style gozero
```

建议使用统一脚本：

```text
scripts/gen-gozero.ps1
```

脚本可以支持：

```powershell
.\scripts\gen-gozero.ps1
.\scripts\gen-gozero.ps1 -Service auth
.\scripts\gen-gozero.ps1 -Service gateway
.\scripts\gen-gozero.ps1 -Service judge-api
```

GoLand 中可以配置：

```text
External Tools
File Watcher
```

让 `.api` 文件保存后自动执行 goctl。

注意：

```text
不要监听 *.go
只监听 services/*/*.api
```

否则会出现生成文件触发 watcher、watcher 再生成文件的循环。

生成出来的 `handler / logic / types / routes` 是源码，应进入版本管理。

---

## 十九、Git 版本管理规则

应该提交：

```text
README.md
docs/**
deploy/compose/docker-compose.yml
deploy/migrations/*.sql

services/auth/**
services/gateway/**
services/judge-api/**
services/shared/**
services/judge-worker/src/**
services/judge-worker/config/**
services/judge-worker/Cargo.toml
services/judge-worker/Cargo.lock

frontend/package.json
frontend/package-lock.json
frontend/src/**
frontend/public/**
frontend/vite.config.ts
frontend/tsconfig*.json
```

不应该提交：

```text
frontend/node_modules/
services/judge-worker/target/
services/*/*.exe
*.log
.env
tmp/
dist/
build/
```

确认未跟踪文件：

```powershell
git status -uall
git ls-files --others --exclude-standard
```

确认被忽略文件：

```powershell
git status --ignored -uall
git check-ignore -v frontend/node_modules
git check-ignore -v services/judge-worker/target
```

确认 NATS 已清理：

```powershell
Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

如果 `Cargo.lock` 中出现：

```text
event-listener
```

这是 Redis / async 依赖链正常内容，不是 NATS。

---

## 二十、当前完成情况

| 模块                         | 状态                    |
| -------------------------- | --------------------- |
| Docker Compose             | 完成                    |
| PostgreSQL                 | 完成                    |
| Redis                      | 完成                    |
| Jaeger                     | 完成                    |
| NATS                       | 已移除                   |
| Migration                  | 完成                    |
| Shared                     | v0.3+ 完成              |
| Gateway                    | v0.3+ 完成              |
| Auth                       | v0.2+ 完成              |
| Permission Core            | v1 完成                 |
| Judge API                  | MVP v0.3+ 完成          |
| Judge Worker               | Reliability v0.3 完成   |
| Judge Queue                | Redis Streams v0.3 完成 |
| 多语言评测配置                    | MVP 完成                |
| Gateway 用户上下文透传            | 完成                    |
| Judge API 权限检查             | judge.submit 已接入      |
| Judge PENDING 恢复           | 完成                    |
| Judge 原子抢任务                | 完成                    |
| Redis XACK                 | 完成                    |
| 安全沙箱                       | 未完成                   |
| 多题型系统                      | 未完成                   |
| 子任务 / 捆绑点                  | 未完成                   |
| Permission 管理 API / UI     | 未完成                   |
| Problem Core               | 未完成                   |
| Contest Core               | 未完成                   |
| Module Registry / Launcher | 未完成                   |

---

## 二十一、当前不是生产级系统的部分

当前仍然缺少：

```text
统一 JSON 错误响应
Problem Core / Dataset Core
problem-api
题目数据文件化
Special Judge
Checker 抽象
Runner Core 抽象
安全沙箱
真实内存限制
进程数限制
网络隔离
文件系统隔离
子任务
捆绑点
交互题
通信题
提交答案题
contest-core
contest-rule-acm
scoreboard-acm
permission-api
权限管理前端
module-registry
feature-flag-core
launcher
```

当前最不应该忽视的是：

```text
Runner 安全隔离
```

因为后续做 SPJ、交互题、通信题时，如果仍然让用户代码直接在 judge-worker 容器内运行，风险会明显扩大。

---

## 二十二、下一阶段计划

推荐下一阶段开发顺序：

```text
1. 清理 NATS 残留，确认源码 / 配置 / Compose / 依赖中不再包含 NATS
2. 更新 README / Docs，记录 Redis Streams Judge Queue
3. 统一错误响应，尤其是 forbidden -> JSON
4. Problem Core / Dataset Core 正规化
5. problem-api 接入 Permission Core
6. 创建 problem 后自动绑定 problem_owner
7. 将 judge-api 中的 problem / test-case 管理接口迁移到 problem-api
8. Runner Core 抽象与基础安全隔离
9. 测试数据文件化
10. checker / special judge 抽象
11. 子任务 / 捆绑点
12. problem-type-traditional
13. contest-core
14. contest-rule-acm
15. scoreboard-acm
16. module-registry
17. feature-flag-core
18. launcher / 模块安装器
```

短期最建议进入：

```text
1. 清理 NATS 残留
2. 统一错误响应
3. Problem Core / Dataset Core 正规化
4. problem-api 接入 Permission Core
```

当前不建议立刻做：

```text
contest-core
module-registry
feature-flag-core
launcher
```

原因是它们依赖：

```text
稳定题目模型
稳定评测模型
稳定权限模型
稳定路由注册方式
稳定模块边界
```

现在最应该先稳定的是：

```text
错误响应
Problem Core
Dataset Core
Runner Core
```

---

## 二十三、长期架构方向

长期推荐架构为：

```text
Platform Kernel
    ↓
Domain Core Modules
    ↓
Capability Modules
    ↓
Adapter Modules
    ↓
Launcher / Module Registry
```

### Platform Kernel

包括：

```text
gateway
auth
permission
shared
module-registry
feature-flag-core
observability
```

### Domain Core Modules

包括：

```text
problem-core
judge-core
runner-core
contest-core
scoreboard-core
team-core
storage-core
```

### Capability Modules

包括：

```text
problem-type-traditional
problem-type-interactive
problem-type-communication
problem-type-output-only

contest-rule-acm
contest-rule-oi
contest-rule-ioi
contest-rule-noi
contest-rule-heuristic

checker-standard
checker-special
scorer-heuristic

balloon-service
print-service
forum-service
clarification-service
```

### Adapter Modules

包括：

```text
icpctools-adapter
polygon-adapter
import-export-adapter
vjudge-adapter
remote-oj-adapter
```

### Launcher / Module Registry

负责：

```text
模块发现
模块依赖检查
模块安装
模块启用
模块禁用
Gateway 路由注册
权限点注册
资源类型注册
数据库迁移注册
前端入口注册
```

---

## 二十四、项目当前结论

OJOS 当前已经完成：

```text
基础设施
认证
网关
权限核心
Judge MVP
Redis Streams Judge Queue
多语言配置
可靠 PENDING 恢复
原子抢任务
```

当前已经具备继续开发以下模块的基础：

```text
Problem Core
Dataset Core
Runner Core
Contest Core
Scoreboard Core
Permission API
Module Registry
Launcher
Frontend
```

当前系统已经不是“空架子”，而是一个可以真实登录、真实鉴权、真实提交、真实判题、真实返回结果的 OJ 原型。

下一阶段重点应从：

```text
能跑通
```

转向：

```text
模型稳定
边界稳定
错误响应稳定
执行安全稳定
数据管理稳定
```

在继续做新功能之前，应优先保证：

```text
NATS 清理干净
文档与当前状态一致
Git 版本管理完整
go build / cargo build / npm build 全部通过
Redis Streams Judge Queue 验收通过
```

完成这些后，再进入：

```text
Problem Core / Dataset Core
```

这是后续支持：

```text
SPJ
子任务
捆绑点
交互题
通信题
提交答案题
OI / NOI / IOI / ACM
比赛榜单
题库权限
模块化安装
```

的前置基础。
