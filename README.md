# Untitled-OJ / OJOS

> OJOS: Online Judge Operating System
> 一个面向模块化、事件驱动、可扩展架构设计的现代 Online Judge 基础设施平台。

---

## 一、项目定位

Untitled-OJ 当前定位不是一个传统单体 Online Judge 网站，而是一个：

```text
OJ Operating System（OJOS）
```

也就是：

```text
Online Judge Infrastructure Platform
```

它的目标不是只实现“能提交代码、能返回 AC/WA”的普通 OJ，而是构建一个可以长期演进、可以按模块安装、可以支持多题型、多赛制、多运行器、多权限范围、多比赛运营能力的 OJ 基础设施平台。

OJOS 的核心设计理念是：

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
Runner 是模块
Checker 是模块
Scorer 是模块
赛制是模块
榜单是模块
气球是模块
打印是模块
Clarification 是模块
Module Registry / Launcher 也是模块
```

但模块化不等于“每个能力都必须拆成一个 Docker 容器”。OJOS 的模块需要分成：

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
problem-api
judge-api
judge-worker
contest-api
scoreboard-api
launcher
```

能力级模块可以嵌入在某个核心服务内部，例如：

```text
problem-type-traditional
problem-type-interactive
traditional-runner
interactive-runner
default-trim-checker
special-checker
default-sum-scorer
contest-rule-acm
contest-rule-ioi
```

这种设计可以避免把系统拆成几十个微服务，导致本地开发、日志排查、网络调用、数据库一致性全部变得不可控。

---

## 二、当前总体状态

当前 OJOS 已经完成了从“基础设施空壳”到“可运行、安全隔离判题原型”的阶段跨越。

目前已经完成的核心能力包括：

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
Problem API 基础 CRUD
题目包文件化存储
Judge API 提交 / 查询 / cancel / rejudge
Rust Judge Worker
Redis Streams Judge Queue
nsjail 沙箱编译与运行
submission 文件化存储
result.json 完整评测结果落盘
default-trim-checker
default-sum-scorer
Gateway JWT 鉴权
可信用户上下文透传
Judge API 权限检查
Judge Worker PENDING 兜底扫描
Judge Worker 原子抢任务
Redis Stream 消费确认
```

当前可以认为已经完成：

```text
Infrastructure Foundation
Shared Core
Gateway Core
Auth Core
Permission Core v1
Problem API v0.1
Judge API v0.4
Judge Worker v0.4
Judge Queue Redis Streams
Package-based Judge Pipeline
nsjail Sandbox Pipeline
```

当前系统已经可以跑通：

```text
用户注册
用户登录
JWT 签发
Gateway 验证 JWT
Gateway 注入可信用户上下文
业务服务读取可信用户身份
Judge API 检查 judge.submit 权限
Problem API 管理题目包
Judge API 创建 submission
Judge API 将源码写入 storage/submissions
Judge API 写入 Redis Stream
Judge Worker 消费 Redis Stream
Judge Worker 原子抢占 submission
Judge Worker 读取 problem.yaml / tests/cases.yaml
Judge Worker 使用 nsjail 编译代码
Judge Worker 使用 nsjail 按测试点运行代码
Judge Worker 使用 default-trim-checker 判定输出
Judge Worker 写入 result.json
Judge Worker 更新 submissions 摘要
用户查询提交结果和 case 结果
```

当前已经完成的真实验收包括：

```text
AC 正常
WA 正常
COMPILE_ERROR 正常
RUNTIME_ERROR 正常
TIME_LIMIT_EXCEEDED 正常
末尾空格和末尾空行忽略正常
行内空格不同判 WRONG_ANSWER 正常
用户程序无法读取题目答案文件
cancel 单份提交正常
rejudge problem 会重测该题全部提交，包括 CANCELLED
/submissions/:id/cases 从 result.json 正常读取
Redis Stream 消息可以被 worker 消费并 XACK
```

当前仍然不是生产级完整 OJ。原因是：

```text
memory_kb 当前暂未接入 cgroup v2 峰值统计
scorer 当前只有默认 sum scorer
runner 当前主要验证 traditional runner
checker 当前主要验证 default-trim-checker
多语言只完成基础配置，仍需逐种语言验收
交互题 / 通信题 / 提交答案题尚未实现
contest-core 尚未实现
scoreboard-core 尚未实现
module-registry / launcher 尚未实现
权限核心已完成，但还没有权限管理 API / UI
```

因此，当前系统定义为：

```text
可运行、安全隔离的 OJ 核心原型
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
| Judge Sandbox         | nsjail                             |
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

当前已经从旧的：

```text
NATS Core Pub/Sub
```

迁移为：

```text
Redis Streams Reliable Queue
```

当前 Judge 任务链路不再依赖 NATS。

Judge 队列使用：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

---

## 四、当前 Monorepo 结构

推荐项目结构如下：

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
│   ├── problem-api/
│   ├── judge-api/
│   └── judge-worker/
│
├── deploy/
│   ├── compose/
│   │   └── docker-compose.yml
│   ├── migrations/
│   └── observability/
│
├── storage/
│   ├── problems/
│   └── submissions/
│
├── docs/
│   ├── architecture.md
│   ├── database.md
│   ├── deployment.md
│   │
│   ├── problem/
│   │   └── package-format.md
│   │
│   ├── judge/
│   │   ├── overview.md
│   │   ├── api.md
│   │   ├── worker.md
│   │   ├── sandbox.md
│   │   ├── submission-storage.md
│   │   ├── result-format.md
│   │   └── validation.md
│   │
│   └── changelog/
│       └── judge-nsjail-pipeline.md
│
├── proto/
├── scripts/
├── README.md
└── .gitignore
```

当前核心服务为：

```text
services/shared
services/gateway
services/auth
services/problem-api
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
判题执行逻辑
```

---

### 5.2 gateway

`services/gateway` 是统一 HTTP 入口。

它负责：

```text
监听 8080
提供 /health
通过配置代理 /api/auth
通过配置代理 /api/problem
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

Gateway 只负责判断请求是否已登录，并将用户身份透传给 `judge-api`。至于该用户是否拥有：

```text
judge.submit @ system:0
```

由 `judge-api` 调用 Permission Core 判断。

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

当前已经接入的真实权限检查包括：

```text
POST /judge/submissions
    -> judge.submit @ system:0

POST /judge/submissions/:id/cancel
    -> problem.manage.data @ problem:<problem_id>

POST /judge/problems/:id/rejudge
    -> problem.manage.data @ problem:<problem_id>
```

---

### 5.5 problem-api

`services/problem-api` 是题目与题目包管理服务。

它负责：

```text
创建题目
查询题目
更新题目
删除题目
管理题目包目录
写入 problem.yaml
写入 statement
写入 tutorial
写入 tests/cases.yaml
写入 tests/groups.yaml
写入 checker / runner / scorer 配置
维护 problems.package_dir
```

题目数据不再直接塞入 Judge 数据库表中，而是文件化存储在：

```text
storage/problems/{id}-{slug}/
```

当前题目包核心结构为：

```text
storage/problems/{id}-{slug}/

├── problem.yaml
├── statement/
├── tests/
│   ├── cases.yaml
│   ├── groups.yaml
│   ├── 001.in
│   └── 001.ans
├── checker/
├── runner/
├── scorer/
└── tutorial/
```

---

### 5.6 judge-api

`services/judge-api` 是 Judge 的 HTTP API 层。

它负责：

```text
创建提交
查询提交摘要
查询提交 case 结果
cancel 单份提交成绩
rejudge 某题全部提交
读取 Gateway 注入用户上下文
检查 judge.submit 权限
检查 problem.manage.data 权限
将提交源码写入 storage/submissions
写入 submissions 摘要
向 Redis Stream 投递判题任务
```

当前接口：

```http
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
POST /judge/submissions/:id/cancel
POST /judge/problems/:id/rejudge
```

`judge-api` 不再负责：

```text
创建题目
添加测试点
管理题面
管理题目数据
执行代码
比较输出
写入每个 case 的数据库记录
```

以下旧接口已经废弃：

```http
POST /judge/problems
POST /judge/test-cases
```

题目与测试数据管理已经迁移到：

```text
problem-api
```

完整 case 结果不再写入 `submission_cases`，而是写入：

```text
storage/submissions/{submission_id}/result.json
```

---

### 5.7 judge-worker

`services/judge-worker` 是 Rust 判题执行器。

它不是 HTTP 服务，而是后台任务进程。

它负责：

```text
连接 PostgreSQL
连接 Redis
加载 languages.yaml
确保 Redis Consumer Group 存在
启动时扫描 PENDING submissions
定时扫描 PENDING submissions
通过 XREADGROUP 消费 Redis Stream
解析 submission_id
try_claim_submission
读取 submissions.code_path
读取 problems.package_dir
加载 problem.yaml
加载 tests/cases.yaml
使用 nsjail 编译用户代码
使用 nsjail 按 case 运行用户程序
执行 default-trim-checker
执行默认分数汇总
写入 stdout / stderr / checker.log
写入 result.json
更新 submissions 摘要
XACK Redis Stream 消息
```

当前 Judge Worker 使用：

```text
Redis Streams
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
+
nsjail 沙箱
```

实现可靠判题任务处理。

---

## 六、Judge 数据流

当前一次提交的完整数据流如下：

```text
User
  ↓
Gateway /api/judge/submissions
  ↓
judge-api
  ↓
检查 JWT 用户上下文
  ↓
检查 judge.submit @ system:0
  ↓
读取 problems.package_dir
  ↓
创建 submissions 记录
  ↓
写入 storage/submissions/{submission_id}/source/main.cpp
  ↓
写入 result.json 初始文件
  ↓
XADD ojos:judge:submissions
  ↓
judge-worker
  ↓
XREADGROUP 消费消息
  ↓
try_claim_submission: PENDING -> JUDGING
  ↓
读取 submissions.code_path
  ↓
读取 problems.package_dir
  ↓
读取 problem.yaml / tests/cases.yaml
  ↓
nsjail 编译
  ↓
每个 case 独立 nsjail 运行
  ↓
default-trim-checker
  ↓
写入 stdout.txt / stderr.txt / checker.log
  ↓
写入 result.json
  ↓
更新 submissions 摘要
  ↓
XACK Redis Stream 消息
```

---

## 七、文件存储模型

### 7.1 题目包存储

题目包位于：

```text
storage/problems/{id}-{slug}/
```

例如：

```text
storage/problems/2-a-plus-b/
```

核心文件：

```text
problem.yaml
tests/cases.yaml
tests/groups.yaml
tests/*.in
tests/*.ans
checker/checker.yaml
runner/runner.yaml
scorer/scorer.yaml
tutorial/zh-cn.md
tutorial/std.cpp
statement/zh-cn.md
```

### 7.2 提交存储

提交文件位于：

```text
storage/submissions/{submission_id}/
```

例如：

```text
storage/submissions/20/
```

核心结构：

```text
storage/submissions/{submission_id}/

├── source/
│   └── main.cpp
│
├── build/
│   ├── main
│   ├── compile.log
│   ├── compile.stdout.log
│   └── compile.stderr.log
│
├── cases/
│   └── 001/
│       ├── stdin.txt
│       ├── stdout.txt
│       ├── stderr.txt
│       └── checker.log
│
└── result.json
```

其中：

```text
source/      保存用户源码
build/       保存编译产物和编译日志
cases/       保存每个测试点的运行输入输出和 checker 日志
result.json  保存完整评测结果
```

数据库中的 `submissions` 表只保存摘要与路径，不保存源码正文和完整 case 结果。

---

## 八、数据库模型总览

当前核心表包括：

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
submissions

schema_migrations
```

当前已经删除或废弃：

```text
test_cases
submission_cases
submissions.code
```

`problems` 当前重点字段包括：

```text
id
slug
title
statement
tutorial
time_limit_ms
memory_limit_mb
package_dir
visibility
status
owner_id
created_at
updated_at
```

`submissions` 当前重点字段包括：

```text
id
problem_id
user_id
language
status
score
time_ms
memory_kb
message
code_path
code_sha256
result_path
judged_at
cancelled_at
cancelled_by
cancel_reason
created_at
updated_at
```

说明：

```text
code_path 指向 storage/submissions/{id}/source/*
result_path 指向 storage/submissions/{id}/result.json
code_sha256 用于源码内容校验和去重辅助
memory_kb 当前暂为 0，后续通过 cgroup v2 统计峰值内存
```

---

## 九、nsjail 沙箱边界

Judge Worker 当前使用 `nsjail` 执行编译和运行。

当前安全边界：

```text
用户程序运行在独立 mount namespace
用户程序运行在独立 pid namespace
用户程序运行在独立 ipc namespace
用户程序运行在独立 uts namespace
用户程序运行在独立 net namespace
用户程序 uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
用户程序不能读取 *.ans
用户程序不能覆盖题目 *.in / *.ans
每个测试点独立运行目录
运行时 stdin/stdout/stderr 通过 /work 内文件重定向
```

Docker Compose 中 `judge-worker` 不使用：

```text
privileged: true
```

但需要最小 capability 支持 nsjail：

```text
SYS_ADMIN
SYS_CHROOT
SETUID
SETGID
NET_ADMIN
```

当前已验证：

```text
jail 内 /data/ojos/problems 不存在
jail 内 uid=10001 gid=10001
/work 可写
用户程序尝试读取 /data/ojos/problems/.../*.ans 会失败
```

当前限制：

```text
memory_kb 暂未采集
内存限制当前主要通过 rlimit_as 实现
后续应接入 cgroup v2 做内存峰值统计
```

---

## 十、基础设施

### 10.1 Docker Compose

当前使用 Docker Compose 编排本地开发环境。

当前基础设施包括：

```text
PostgreSQL
Redis
Jaeger
Gateway
Auth
Problem API
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
docker compose ps
```

查看日志：

```powershell
docker logs ojos-gateway
docker logs ojos-auth
docker logs ojos-problem-api
docker logs ojos-judge-api
docker logs ojos-judge-worker
docker logs ojos-redis
docker logs ojos-postgres
```

重建单个服务：

```powershell
docker compose build judge-worker
docker compose up -d judge-worker
```

或：

```powershell
docker compose build judge-api
docker compose up -d --no-deps judge-api
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

### 10.2 PostgreSQL

当前数据库名：

```text
ojos
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

查看提交：

```sql
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    score,
    time_ms,
    memory_kb,
    code_path,
    result_path,
    judged_at
FROM submissions
ORDER BY id DESC
LIMIT 10;
```

---

### 10.3 Migration

当前使用：

```text
golang-migrate
```

执行迁移：

```powershell
cd D:\Untitled-OJ

migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

查看版本：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  version
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
  -seq <migration_name>
```

当前迁移原则：

```text
已经被执行过的 migration 不要随意重写历史
新增结构应通过新的 migration 引入
down 文件应谨慎，避免误删核心数据
开发阶段可以清库重建，但仍应保持 migration 可重复执行
```

---

### 10.4 Redis

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
submission_id 20
created_at    2026-06-04T13:28:02Z
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

### 10.5 Jaeger / OpenTelemetry

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
Problem API tracing
Judge API tracing
Shared tracing 初始化
HTTP middleware trace_id / span_id 注入日志
```

后续仍需完善：

```text
采样率配置
超时控制
失败降级
BatchSpanProcessor
Redis queue trace propagation
Judge Worker trace span
跨服务 trace 串联验收
```

---

## 十一、当前 API 概览

### 11.1 Gateway

```http
GET /health
```

### 11.2 Auth

Gateway 暴露：

```text
/api/auth/*
```

内部路径：

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

### 11.3 Problem API

Gateway 暴露：

```text
/api/problem/*
```

当前主要负责：

```text
题目 CRUD
题目包生成
测试点与数据文件管理
```

具体接口以 `services/problem-api/problemapi.api` 为准。

### 11.4 Judge API

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
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
POST /judge/submissions/:id/cancel
POST /judge/problems/:id/rejudge
```

---

## 十二、当前 Judge 验收命令

### 12.1 登录

```powershell
$body = @{
  username = "permtest"
  password = "123456"
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/login" `
  -ContentType "application/json; charset=utf-8" `
  -Body ([System.Text.Encoding]::UTF8.GetBytes($body))

$token = $res.data.token
$headers = @{ Authorization = "Bearer $token" }
```

### 12.2 提交代码

```powershell
$submitObj = @{
  problem_id = 2
  language = "cpp17"
  code = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    long long a, b;
    cin >> a >> b;
    cout << a + b << '\n';
    return 0;
}
'@
}

$json = $submitObj | ConvertTo-Json -Compress
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)

$sub = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/submissions" `
  -ContentType "application/json; charset=utf-8" `
  -Headers $headers `
  -Body $bytes

$sub
```

预期：

```text
status = PENDING
code_path 非空
result_path 非空
```

### 12.3 查询结果

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/$($sub.submission_id)" `
  -Headers $headers
```

预期：

```text
status = ACCEPTED
score = 100
```

### 12.4 查看 result.json

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($sub.submission_id)\result.json" -Encoding UTF8
```

### 12.5 查询 case 结果

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/$($sub.submission_id)/cases" `
  -Headers $headers
```

---

## 十三、本地编译

### 13.1 Shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 13.2 Auth

```powershell
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 13.3 Gateway

```powershell
cd D:\Untitled-OJ\services\gateway

go mod tidy
go build .
```

### 13.4 Problem API

```powershell
cd D:\Untitled-OJ\services\problem-api

go mod tidy
go build .
```

### 13.5 Judge API

```powershell
cd D:\Untitled-OJ\services\judge-api

go mod tidy
go build .
```

### 13.6 Judge Worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo check
cargo build
```

### 13.7 Frontend

```powershell
cd D:\Untitled-OJ\frontend

npm install
npm run dev
npm run build
```

---

## 十四、go-zero 生成流程

Auth、Gateway、Problem API、Judge API 都是 go-zero API 服务。

`.api` 文件修改后，需要重新生成：

```powershell
cd D:\Untitled-OJ\services\auth
goctl api go -api auth.api -dir . --style gozero

cd D:\Untitled-OJ\services\gateway
goctl api go -api gateway.api -dir . --style gozero

cd D:\Untitled-OJ\services\problem-api
goctl api go -api problemapi.api -dir . --style gozero

cd D:\Untitled-OJ\services\judge-api
goctl api go -api judgeapi.api -dir . --style gozero
```

建议使用统一脚本：

```text
scripts/gen-gozero.ps1
```

生成出来的 `handler / logic / types / routes` 是源码，应进入版本管理。

注意：

```text
不要监听 *.go
只监听 services/*/*.api
```

否则会出现生成文件触发 watcher、watcher 再生成文件的循环。

---

## 十五、Git 版本管理规则

应该提交：

```text
README.md
docs/**
deploy/compose/docker-compose.yml
deploy/migrations/*.sql

services/auth/**
services/gateway/**
services/problem-api/**
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
storage/problems/*
storage/submissions/*
```

可以保留：

```text
storage/problems/.gitkeep
storage/submissions/.gitkeep
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
git check-ignore -v storage/submissions
```

确认 NATS 已清理：

```powershell
Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

---

## 十六、当前完成情况

| 模块                         | 状态                            |
| -------------------------- | ----------------------------- |
| Docker Compose             | 完成                            |
| PostgreSQL                 | 完成                            |
| Redis                      | 完成                            |
| Jaeger                     | 完成                            |
| NATS                       | 已移除                           |
| Migration                  | 完成                            |
| Shared                     | 完成                            |
| Gateway                    | 完成                            |
| Auth                       | 完成                            |
| Permission Core            | v1 完成                         |
| Problem API                | 基础 CRUD 完成                    |
| Problem Package            | 基础格式完成                        |
| Judge API                  | 提交 / 查询 / cancel / rejudge 完成 |
| Judge Worker               | nsjail pipeline 完成            |
| Judge Queue                | Redis Streams 完成              |
| 多语言评测配置                    | 基础配置完成                        |
| Gateway 用户上下文透传            | 完成                            |
| Judge API 权限检查             | 完成                            |
| Judge PENDING 恢复           | 完成                            |
| Judge 原子抢任务                | 完成                            |
| Redis XACK                 | 完成                            |
| nsjail 安全沙箱                | 基础完成                          |
| default-trim-checker       | 完成                            |
| default-sum-scorer         | 完成                            |
| result.json                | 完成                            |
| memory_kb 统计               | 未完成                           |
| 多题型系统                      | 未完成                           |
| 子任务 / 捆绑点                  | 未完成                           |
| Permission 管理 API / UI     | 未完成                           |
| Contest Core               | 未完成                           |
| Module Registry / Launcher | 未完成                           |

---

## 十七、当前不是生产级系统的部分

当前仍然缺少：

```text
统一 JSON 错误响应
完整 Problem Core / Dataset Core
Special Judge
Checker 插件化
Runner 插件化
Scorer 插件化
真实 memory_kb 峰值统计
cgroup v2 内存统计
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
Runner / Checker / Scorer 的稳定抽象
memory 统计
多语言逐项验收
```

---

## 十八、下一阶段计划

推荐下一阶段开发顺序：

```text
1. 重构并补全文档
2. 多语言验收：c11 / python3 / java17
3. memory_kb 接入 cgroup v2 统计
4. scorer 抽象：ACM / IOI / Subtask / Bundle
5. checker 抽象：default / special judge
6. runner 抽象：traditional / interactive / communication / output-only
7. Problem Core / Dataset Core 深化
8. Contest Core
9. contest-rule-acm
10. scoreboard-acm
11. permission-api
12. module-registry
13. launcher / 模块安装器
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
文档结构
Problem Package
Judge Pipeline
Runner / Checker / Scorer 抽象
内存统计
```

---

## 十九、文档导航

当前推荐文档结构：

```text
docs/

├── architecture.md
├── database.md
├── deployment.md
│
├── problem/
│   └── package-format.md
│
├── judge/
│   ├── overview.md
│   ├── api.md
│   ├── worker.md
│   ├── sandbox.md
│   ├── submission-storage.md
│   ├── result-format.md
│   └── validation.md
│
└── changelog/
    └── judge-nsjail-pipeline.md
```

其中：

```text
README.md
```

只作为项目入口和状态总览，不继续堆积所有实现细节。

详细设计应拆到：

```text
docs/architecture.md
docs/problem/package-format.md
docs/judge/*
docs/database.md
docs/deployment.md
```

---

## 二十、项目当前结论

OJOS 当前已经完成：

```text
基础设施
认证
网关
权限核心
Problem API 基础能力
题目包文件化
Judge API
Redis Streams Judge Queue
nsjail Judge Worker
提交文件化存储
result.json
default-trim-checker
default-sum-scorer
可靠 PENDING 恢复
原子抢任务
```

当前系统已经不是“空架子”，而是一个可以真实登录、真实鉴权、真实建题、真实提交、真实沙箱判题、真实返回结果的 OJ 核心原型。

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
Runner / Checker / Scorer 抽象稳定
```

完成文档重构和 Judge 子系统收口后，再进入：

```text
Scorer / Runner / Checker 深化
Problem Core / Dataset Core 深化
Contest Core
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
