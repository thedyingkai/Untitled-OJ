# Untitled-OJ / OJOS

> OJOS: Online Judge Operating System
> 一个面向模块化、事件驱动、可扩展架构设计的现代 Online Judge 基础设施平台。

---

## 一、项目定位

Untitled-OJ 当前定位不是传统单体 Online Judge，而是一个：

```text
OJ Operating System（OJOS）
```

即：

```text
Online Judge Infrastructure Platform
```

项目目标是构建一个具备以下特性的 OJ 平台：

```text
高性能
分布式
模块化
事件驱动
可观测
可扩展
支持多语言评测
支持多服务独立演进
```

核心理念：

```text
Everything is a Module
Everything is Event-Driven
Everything is Extensible
```

---

## 二、当前总体状态

当前 OJOS 已经完成从“基础设施空壳”到“可运行 OJ 原型”的第一阶段跨越。

目前已经具备：

```text
基础设施编排
数据库迁移
认证服务
统一网关
公共基础库
真实判题闭环
多语言评测配置
日志与链路追踪
```

当前可以认为已经完成：

```text
Infrastructure Foundation
Auth MVP
Gateway MVP
Judge MVP
Shared Common Library
```

但仍然不是生产级完整 OJ。当前 Judge 仍处于 MVP 阶段，用户代码尚未通过独立安全沙箱隔离执行。

---

## 三、技术栈

| 模块                     | 技术                               |
| ---------------------- | -------------------------------- |
| Backend API            | Go                               |
| API Framework          | go-zero                          |
| Judge Worker           | Rust                             |
| Judge Language Config  | YAML                             |
| Database               | PostgreSQL 17                    |
| DB Driver              | pgx / pgxpool                    |
| Migration              | golang-migrate                   |
| Message Queue          | NATS                             |
| Cache                  | Redis                            |
| Tracing                | OpenTelemetry                    |
| Trace UI               | Jaeger                           |
| Logger                 | Zap                              |
| Deployment             | Docker Compose                   |
| Auth                   | JWT / bcrypt                     |
| C++ Judge Toolchain    | g++                              |
| Other Judge Toolchains | gcc / python3 / Java / Go / Rust |

---

## 四、当前 Monorepo 结构

```text
Untitled-OJ/

├── frontend/
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
│   ├── migrations/
│   └── observability/
│
├── docs/
│
├── proto/
│
└── scripts/
```

---

## 五、服务模块概览

当前核心服务包括：

```text
shared        公共基础库
gateway       统一 HTTP 网关
auth          认证服务
judge-api     判题 API 服务
judge-worker  Rust 判题执行器
```

---

## 六、基础设施

### 6.1 Docker Compose

当前使用 Docker Compose 编排本地开发环境。

已接入容器：

```text
PostgreSQL
Redis
NATS
Jaeger
Gateway
Auth
Judge API
Judge Worker
```

启动命令：

```powershell
cd deploy/compose

docker compose up -d --build
```

查看运行状态：

```powershell
docker ps
```

---

### 6.2 PostgreSQL

当前数据库：

```text
ojos
```

PostgreSQL 用于存储：

```text
用户
角色
题目
测试点
提交记录
测试点评测结果
```

当前核心表：

```text
users
roles
user_roles
problems
test_cases
submissions
submission_cases
schema_migrations
```

---

### 6.3 Migration

使用：

```text
golang-migrate
```

当前 migration：

```text
000001_init_schema
000002_judge_schema
```

执行迁移：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

当前已验证：

```text
schema_migrations.version = 2
schema_migrations.dirty = false
```

---

### 6.4 Redis

Redis 当前已经 Docker 化部署。

当前主要作为基础设施预留，后续用于：

```text
排行榜缓存
比赛缓存
Session
WebSocket 状态
限流
热点数据缓存
```

---

### 6.5 NATS

NATS 当前用于服务间事件通信。

当前已使用事件：

```text
submission.created
```

当前 Judge 链路：

```text
judge-api
    ↓
NATS submission.created
    ↓
judge-worker
```

当前限制：

```text
当前使用 NATS Core Pub/Sub
消息不持久
worker 离线期间可能丢任务
```

后续需要引入：

```text
PENDING 任务扫描
或 NATS JetStream
```

---

### 6.6 Jaeger / OpenTelemetry

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
Shared tracing 初始化
HTTP middleware trace_id / span_id 注入日志
```

---

## 七、Shared 公共模块

路径：

```text
services/shared
```

Shared 是公共基础库，不是独立 HTTP 服务。

当前 Shared 已完成 go-zero 适配和旧兼容层清理。

当前目录：

```text
services/shared/

├── database/
├── events/
├── logger/
├── middleware/
├── tracing/
├── go.mod
└── go.sum
```

已删除旧模块：

```text
shared/config
shared/response
```

Shared 当前提供：

```text
PostgreSQL URL 初始化
NATS EventBus
统一 Event 结构
Zap logger
trace_id / span_id 日志注入
OpenTelemetry OTLP 初始化
go-zero Recovery Middleware
go-zero Logging Middleware
```

Shared 当前原则：

```text
服务自己定义配置
shared 只接收参数并创建基础设施对象
业务逻辑不进入 shared
业务配置不进入 shared
新增业务模块不修改 shared
```

---

## 八、Gateway 模块

路径：

```text
services/gateway
```

Gateway 是 OJOS 的统一 HTTP 入口。

当前 Gateway 已完成 go-zero 重构。

当前能力：

```text
go-zero 标准结构
/health
shared logger 接入
shared tracing 接入
shared database 接入
shared NATS 接入
Recovery middleware
Logging middleware
配置驱动反向代理
Auth 服务代理
Judge API 服务代理
```

当前监听端口：

```text
8080
```

### 8.1 Gateway API

当前 Gateway 自身接口：

```http
GET /health
```

返回：

```json
{
  "status": "ok"
}
```

### 8.2 配置驱动代理

Gateway 当前通过 `gateway.yaml` 配置代理规则。

示例：

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

转发规则：

```text
/api/auth/login
    -> http://auth:8081/auth/login

/api/judge/submissions/3/cases
    -> http://judge-api:8082/judge/submissions/3/cases
```

当前已验证：

```text
GET  /health
POST /api/auth/login
GET  /api/auth/profile
GET  /api/judge/submissions/3/cases
```

### 8.3 当前限制

Gateway 当前仍未实现：

```text
统一 JWT 鉴权
X-User-Id / X-Username / X-Roles 透传
限流
熔断
重试
服务发现
配置热更新
统一响应格式
```

---

## 九、Auth 模块

路径：

```text
services/auth
```

Auth 是认证服务，负责：

```text
用户注册
用户登录
密码哈希
JWT 签发
JWT 解析
用户 Profile
角色读取
```

当前 Auth 已完成 go-zero 重构。

当前监听端口：

```text
8081
```

### 9.1 Auth API

当前接口：

```http
GET  /health
POST /auth/register
POST /auth/login
GET  /auth/profile
```

其中：

```http
GET /auth/profile
```

受 JWT 中间件保护。

### 9.2 Gateway 访问方式

通过 Gateway：

```http
POST /api/auth/login
GET  /api/auth/profile
```

Gateway 会转发到：

```text
auth:8081
```

### 9.3 JWT

当前 JWT Claims 至少包含：

```text
user_id
username
roles
iss
sub
exp
iat
```

当前开发配置：

```yaml
Jwt:
  Secret: ojos-dev-secret-change-me
  ExpireHours: 24
```

生产环境必须替换为强随机密钥。

### 9.4 RBAC 基础表

当前数据库角色系统：

```text
users
roles
user_roles
```

默认角色：

| 角色          | 描述      |
| ----------- | ------- |
| super_admin | 系统超级管理员 |
| admin       | 管理员     |
| user        | 普通用户    |

当前新注册用户默认分配：

```text
user
```

### 9.5 当前限制

Auth 当前未实现：

```text
refresh token
token revoke
登出
修改密码
权限点表
角色管理接口
Gateway 统一鉴权
```

---

## 十、Judge API 模块

路径：

```text
services/judge-api
```

Judge API 是判题系统的 HTTP API 层，使用 go-zero 实现。

当前监听端口：

```text
8082
```

当前接口：

```http
POST /judge/problems
POST /judge/test-cases
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
```

### 10.1 创建题目

```http
POST /judge/problems
```

请求示例：

```json
{
  "title": "A+B Problem",
  "time_limit_ms": 1000,
  "memory_limit_mb": 256
}
```

响应示例：

```json
{
  "problem_id": 1
}
```

---

### 10.2 添加测试点

```http
POST /judge/test-cases
```

请求示例：

```json
{
  "problem_id": 1,
  "input": "1 2\n",
  "output": "3\n",
  "score": 100
}
```

响应示例：

```json
{
  "test_case_id": 1
}
```

---

### 10.3 提交代码

```http
POST /judge/submissions
```

请求示例：

```json
{
  "problem_id": 1,
  "user_id": 1,
  "language": "cpp17",
  "code": "#include <bits/stdc++.h>\nusing namespace std;\nint main(){int a,b;cin>>a>>b;cout<<a+b<<endl;}"
}
```

响应示例：

```json
{
  "submission_id": 1,
  "status": "PENDING"
}
```

提交后，Judge API 会：

```text
写入 submissions
发布 NATS 事件 submission.created
```

---

### 10.4 查询提交结果

```http
GET /judge/submissions/:id
```

响应示例：

```json
{
  "id": 3,
  "problem_id": 1,
  "user_id": 1,
  "language": "cpp17",
  "status": "ACCEPTED",
  "score": 100,
  "time_ms": 4,
  "memory_kb": 0,
  "message": ""
}
```

---

### 10.5 查询测试点详情

```http
GET /judge/submissions/:id/cases
```

响应示例：

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

## 十一、Judge Worker 模块

路径：

```text
services/judge-worker
```

Judge Worker 使用 Rust 实现，是实际执行判题的模块。

当前能力：

```text
连接 NATS
订阅 submission.created
连接 PostgreSQL
读取提交
读取题目配置
读取测试点
根据 languages.yaml 选择语言
编译代码
运行测试点
比较输出
写入 submission_cases
更新 submissions
```

---

### 11.1 languages.yaml

路径：

```text
services/judge-worker/config/languages.yaml
```

语言配置示例：

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

  python3:
    source_file: main.py
    exe_file: ""
    compile:
      enabled: false
      command: ""
      args: []
      timeout_ms: 0
    run:
      command: python3
      args:
        - "{source}"
```

支持占位符：

| 占位符         | 含义      |
| ----------- | ------- |
| `{source}`  | 源文件路径   |
| `{exe}`     | 可执行文件路径 |
| `{workdir}` | 临时工作目录  |

当前配置支持：

```text
cpp17
cpp20
c11
python3
java17
rust
go
```

实际可用语言取决于 judge-worker Docker 镜像中安装的工具链。

---

### 11.2 当前评测状态

当前支持状态：

| 状态                   | 含义     |
| -------------------- | ------ |
| PENDING              | 等待评测   |
| RUNNING              | 正在评测   |
| ACCEPTED             | 通过     |
| WRONG_ANSWER         | 答案错误   |
| COMPILE_ERROR        | 编译错误   |
| RUNTIME_ERROR        | 运行错误   |
| TIME_LIMIT_EXCEEDED  | 超时     |
| SYSTEM_ERROR         | 系统错误   |
| UNSUPPORTED_LANGUAGE | 不支持的语言 |

当前已验证：

```text
AC 正常
WA 正常
CE 正常
TLE 正常
C++17 正常
Python3 正常
submission_cases 正常
```

---

### 11.3 当前 Judge 限制

当前 Judge 是 MVP，不是安全生产级 Judge。

当前限制：

```text
用户代码直接运行在 judge-worker 容器内
没有独立安全沙箱
没有真实内存限制
NATS Core 消息不持久
多 worker 可能重复判题
测试点直接存数据库 TEXT 字段
不支持 Special Judge
不支持 OI 子任务计分
不支持交互题
```

后续必须补：

```text
runner 隔离容器
network none
cpu / memory / pids 限制
PENDING 任务扫描
原子抢任务
测试数据文件化
SPJ / checker
```

---

## 十二、当前数据库表

当前核心数据库表：

```text
schema_migrations
users
roles
user_roles
problems
test_cases
submissions
submission_cases
```

### 12.1 users

存储用户信息。

### 12.2 roles

存储角色信息。

### 12.3 user_roles

存储用户和角色关系。

### 12.4 problems

存储题目信息：

```text
title
time_limit_ms
memory_limit_mb
```

### 12.5 test_cases

存储测试点：

```text
problem_id
input
output
score
```

### 12.6 submissions

存储提交：

```text
problem_id
user_id
language
code
status
score
time_ms
memory_kb
message
```

### 12.7 submission_cases

存储每个测试点的评测结果：

```text
submission_id
test_case_id
status
time_ms
memory_kb
message
```

---

## 十三、当前服务端口

| 服务                   | 端口    |
| -------------------- | ----- |
| Gateway              | 8080  |
| Auth                 | 8081  |
| Judge API            | 8082  |
| Jaeger UI            | 16686 |
| PostgreSQL Host      | 5433  |
| PostgreSQL Container | 5432  |
| NATS                 | 4222  |
| Redis                | 6379  |

---

## 十四、快速启动

### 14.1 启动全部服务

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build
```

### 14.2 查看服务

```powershell
docker ps
```

### 14.3 查看 Gateway 日志

```powershell
docker logs ojos-gateway
```

### 14.4 查看 Auth 日志

```powershell
docker logs ojos-auth
```

### 14.5 查看 Judge Worker 日志

```powershell
docker logs ojos-judge-worker
```

---

## 十五、常用验收命令

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

### 15.2 Auth Login

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

$res
```

预期：

```text
code = 0
msg = success
data.token 存在
```

---

### 15.3 Auth Profile

```powershell
$token = $res.data.token

Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期：

```text
code = 0
msg = success
data.username = admin
```

---

### 15.4 Judge Cases

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/3/cases"
```

预期：

```text
cases.status = ACCEPTED
```

---

## 十六、本地编译

### 16.1 Shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 16.2 Gateway

```powershell
cd D:\Untitled-OJ\services\gateway

go mod tidy
go build .
```

### 16.3 Auth

```powershell
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 16.4 Judge API

```powershell
cd D:\Untitled-OJ\services\judge-api

go mod tidy
go build .
```

### 16.5 Judge Worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

---

## 十七、当前完成情况

| 模块             | 状态          |
| -------------- | ----------- |
| Docker Compose | 完成          |
| PostgreSQL     | 完成          |
| Redis          | 完成          |
| NATS           | 完成          |
| Jaeger         | 完成          |
| Migration      | 完成          |
| Shared         | v0.2 完成     |
| Gateway        | v0.2 完成     |
| Auth           | v0.2 完成     |
| Judge API      | MVP v0.1 完成 |
| Judge Worker   | MVP v0.1 完成 |
| 多语言评测配置        | MVP 完成      |
| 安全沙箱           | 未完成         |
| PENDING 任务兜底   | 未完成         |
| 多 Worker 并发安全  | 未完成         |
| Gateway 统一鉴权   | 未完成         |

---

## 十八、当前不是完整生产级系统的部分

当前系统仍然存在以下关键缺口：

```text
Judge 没有安全沙箱
Judge 没有真实内存限制
NATS 任务消息不持久
多 worker 可能重复评测
Gateway 没有统一 JWT 鉴权
Gateway 没有用户信息透传
测试数据没有文件化
没有 Special Judge
没有 OI 子任务计分
没有完整权限点系统
没有统一错误码体系
```

因此当前系统应定义为：

```text
可运行 OJ 原型 / MVP
```

而不是：

```text
生产级完整 OJ
```

---

## 十九、下一阶段计划

推荐下一阶段开发顺序：

```text
1. Gateway JWT 鉴权
2. Gateway 用户信息透传
3. Judge Worker 原子抢任务
4. Judge Worker 扫描 PENDING
5. Judge Runner 隔离容器
6. CPU / memory / pids / network 限制
7. 测试数据文件化
8. Problem 模块
9. Contest 模块
10. Special Judge
11. OI 计分模式
12. 权限点系统
13. 统一错误码体系
```

优先级最高的是：

```text
Gateway 鉴权
Judge 任务可靠性
Judge 安全隔离
```

---

## 二十、项目当前结论

OJOS 当前已经完成基础平台和核心 MVP：

```text
Gateway 可以作为统一入口
Auth 可以完成登录鉴权
Judge 可以真实评测代码
Shared 已成为纯公共基础库
Docker Compose 可以启动完整本地环境
```

当前系统已经具备继续开发：

```text
Problem
Contest
Training
Ranking
Submission
WebSocket
Frontend
```

等模块的基础。

下一阶段重点应从“能跑通”转向：

```text
安全
可靠
权限
可维护
可扩展
```
