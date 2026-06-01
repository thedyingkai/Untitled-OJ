# OJOS Judge 模块开发文档

## 一、模块定位

Judge 模块是 OJOS 的核心评测模块，负责完成从“用户提交代码”到“返回评测结果”的完整链路。

当前 Judge 模块已经完成 MVP，可以真实完成：

```text
创建题目 MVP
添加测试点 MVP
提交代码
权限检查
写入提交记录
投递判题任务
Rust Worker 消费任务
编译用户代码
运行测试点
比较标准输出
写入测试点结果
写入总评测结果
查询提交结果
查询测试点结果
```

当前 Judge 模块不是最终生产级安全 Judge。

当前已经能真实判题，但仍然存在以下关键限制：

```text
用户代码仍直接运行在 judge-worker 容器内
没有独立 runner sandbox
没有真实 memory limit
没有 network none 隔离
没有 pids 限制
没有文件系统隔离
没有 Special Judge
没有子任务 / 捆绑点
没有交互题
没有通信题
没有提交答案题
```

因此当前 Judge 模块应定义为：

```text
Judge MVP + Redis Streams Reliable Queue
```

而不是：

```text
Production Judge
```

当前 Judge 模块的核心目标是：

```text
证明 OJOS 可以完成真实提交、真实排队、真实执行、真实回写结果
```

后续 Judge 模块要逐步演进为：

```text
Judge Core
Runner Core
Dataset Core
Checker Core
Problem Type Modules
Scoring Modules
```

当前阶段不要把所有东西都继续塞进 `judge-api`。

`judge-api` 目前承担了部分题目和测试点管理能力，这是 MVP 阶段为了打通链路临时保留的设计。后续应迁移到：

```text
problem-api
dataset-core
```

Judge API 最终应该主要负责：

```text
submissions
submission_cases
judge tasks
rejudge tasks
```

---

## 二、当前版本状态

当前 Judge 模块由两个核心服务组成：

```text
services/judge-api
services/judge-worker
```

当前版本可以记为：

```text
Judge API MVP v0.3+
Judge Worker Reliability v0.3
Judge Queue Redis Streams v0.3
```

当前已完成：

```text
judge-api go-zero 标准结构
judgeapi.api 接口定义
创建题目 MVP
添加测试点 MVP
创建提交
查询提交
查询测试点结果
Gateway 用户上下文读取
Permission Core judge.submit 检查
PostgreSQL submissions 写入
Redis Streams XADD 投递任务
judge-worker Rust 实现
Redis XREADGROUP 消费任务
Redis XACK 确认任务
PostgreSQL PENDING 启动扫描
PostgreSQL PENDING 定时扫描
数据库原子抢任务
多语言配置 languages.yaml
编译阶段
运行阶段
标准输出比较
submission_cases 写入
submissions 总结果更新
AC / WA / CE / TLE / RE / SYSTEM_ERROR 基础状态
```

当前已经验证：

```text
普通用户登录后可以提交代码
提交时 user_id 来自 Gateway 注入上下文
请求体中不再信任 user_id
judge.submit 权限检查生效
deny judge.submit 后提交被 forbidden 拦截
Redis Stream 中产生 submission.created 消息
judge-worker 能实时消费 Redis Stream
worker 能 claim submission
worker 能完成编译运行
worker 能写回 ACCEPTED
worker 能 XACK 消息
XPENDING 为 0
worker 启动时能恢复历史 PENDING
已经判完的 submission 再收到 Stream 消息会 skip 并 ACK
```

当前已经删除或不再使用：

```text
NATS
NATS_URL
async-nats
src/event.rs
shared/events
submission.created NATS Publish
NATS Core Pub/Sub judge queue
```

当前 Judge 任务链路已经从旧的：

```text
judge-api
    ↓
NATS submission.created
    ↓
judge-worker
```

迁移为：

```text
judge-api
    ↓
Redis Stream ojos:judge:submissions
    ↓
judge-worker XREADGROUP
```

当前可靠性模型是：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 兜底扫描
+
数据库原子抢任务
+
Redis XACK
```

---

## 三、模块组成

当前 Judge 模块由两个服务组成。

### 3.1 judge-api

路径：

```text
services/judge-api
```

技术栈：

```text
Go
go-zero
pgxpool
Redis client
Permission Core
Gateway User Context
```

职责：

```text
提供 Judge HTTP API
读取 Gateway 注入的用户上下文
检查 judge.submit 权限
写入 submissions
写入 MVP problems
写入 MVP test_cases
查询 submissions
查询 submission_cases
向 Redis Stream 投递判题任务
```

当前监听端口：

```text
8082
```

Gateway 暴露路径：

```text
/api/judge/*
```

内部路径：

```text
/judge/*
```

---

### 3.2 judge-worker

路径：

```text
services/judge-worker
```

技术栈：

```text
Rust
tokio
sqlx
redis crate
tracing
languages.yaml
```

职责：

```text
连接 PostgreSQL
连接 Redis
加载 languages.yaml
确保 Redis Consumer Group 存在
启动扫描 PENDING submissions
定时扫描 PENDING submissions
消费 Redis Stream
解析 submission_id
执行 try_claim_submission
编译用户代码
运行测试点
比较输出
写入 submission_cases
更新 submissions
XACK Redis Stream 消息
```

Judge Worker 不是 HTTP 服务，不监听端口。

它是后台任务进程。

---

## 四、当前目录结构

### 4.1 judge-api 目录结构

当前 `judge-api` 目录结构应为：

```text
services/judge-api/

├── etc/
│   └── judgeapi.yaml
│
├── internal/
│   ├── config/
│   │   └── config.go
│   │
│   ├── handler/
│   │   ├── addtestcasehandler.go
│   │   ├── createproblemhandler.go
│   │   ├── createsubmissionhandler.go
│   │   ├── getsubmissioncaseshandler.go
│   │   ├── getsubmissionhandler.go
│   │   └── routes.go
│   │
│   ├── logic/
│   │   ├── addtestcaselogic.go
│   │   ├── createproblemlogic.go
│   │   ├── createsubmissionlogic.go
│   │   ├── getsubmissioncaseslogic.go
│   │   └── getsubmissionlogic.go
│   │
│   ├── middleware/
│   │   └── usercontextmiddleware.go
│   │
│   ├── repository/
│   │   └── judge_repository.go
│   │
│   ├── svc/
│   │   └── servicecontext.go
│   │
│   └── types/
│       └── types.go
│
├── judgeapi.api
├── judgeapi.go
├── Dockerfile
├── go.mod
└── go.sum
```

说明：

```text
handler / logic / types / routes 由 goctl 生成
repository 是手写数据库访问层
middleware/usercontextmiddleware.go 负责读取 Gateway 注入用户上下文
logic/createsubmissionlogic.go 是当前最关键逻辑
svc/servicecontext.go 负责 DB / Redis / Repo / Middleware 初始化
```

当前不应再出现：

```text
internal/svc 中的 Nats *nats.Conn
internal/config 中的 NatsConfig
createsubmissionlogic.go 中的 Nats.Publish
github.com/nats-io/nats.go
```

---

### 4.2 judge-worker 目录结构

当前 `judge-worker` 目录结构应为：

```text
services/judge-worker/

├── config/
│   └── languages.yaml
│
├── src/
│   ├── config.rs
│   ├── db.rs
│   ├── judge.rs
│   └── main.rs
│
├── Cargo.toml
├── Cargo.lock
└── Dockerfile
```

当前不应再存在：

```text
src/event.rs
```

因为旧 `event.rs` 用于解析 NATS 事件结构，现在已经迁移到 Redis Stream 字段解析。

当前不应再依赖：

```text
async-nats
futures-util
```

如果 `Cargo.toml` 里仍然有：

```toml
async-nats = "..."
futures-util = "..."
```

应删除。

如果 `Cargo.lock` 中出现：

```text
event-listener
```

不用处理，它是 Redis / async 依赖链中的正常依赖，不是 NATS。

---

## 五、go-zero API 结构

Judge API 使用 go-zero API 模式。

接口描述文件：

```text
services/judge-api/judgeapi.api
```

重新生成命令：

```powershell
cd D:\Untitled-OJ\services\judge-api

goctl api go -api judgeapi.api -dir . --style gozero
```

也可以使用统一脚本：

```powershell
cd D:\Untitled-OJ

.\scripts\gen-gozero.ps1 -Service judge-api
```

注意：

```text
goctl 生成文件属于源码
handler / logic / types / routes 应进入 Git
不要把生成文件当成临时文件忽略
```

如果出现残留模板文件，例如：

```text
internal/logic/judgeapilogic.go
internal/handler/judgeapihandler.go
```

并且引用不存在的：

```go
types.Request
types.Response
```

应删除这些残留文件。

---

## 六、Judge API 当前接口

当前 Judge API 接口：

```http
POST /judge/problems
POST /judge/test-cases
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
```

通过 Gateway 访问时：

```http
POST /api/judge/problems
POST /api/judge/test-cases
POST /api/judge/submissions
GET  /api/judge/submissions/:id
GET  /api/judge/submissions/:id/cases
```

当前接口说明：

| 接口                                 | 当前职责      | 后续归属                             |
| ---------------------------------- | --------- | -------------------------------- |
| `POST /judge/problems`             | MVP 创建题目  | 迁移到 `problem-api`                |
| `POST /judge/test-cases`           | MVP 添加测试点 | 迁移到 `dataset-core / problem-api` |
| `POST /judge/submissions`          | 创建提交      | 保留在 `judge-api`                  |
| `GET /judge/submissions/:id`       | 查询提交总结果   | 保留在 `judge-api`                  |
| `GET /judge/submissions/:id/cases` | 查询测试点结果   | 保留在 `judge-api`                  |

当前最核心接口是：

```http
POST /judge/submissions
```

这个接口已经完成：

```text
用户身份绑定
权限检查
提交写库
Redis Stream 投递
```

---

## 七、judge-api 配置

路径：

```text
services/judge-api/etc/judgeapi.yaml
```

当前推荐配置：

```yaml
Name: judge-api-service
Host: 0.0.0.0
Port: 8082

Database:
  Url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

Redis:
  Url: redis://ojos-redis:6379/0

Jaeger:
  Endpoint: ojos-jaeger:4317
```

如果当前 `judge-api` 尚未接入 Jaeger，也可以暂时没有 `Jaeger` 字段，但推荐后续统一接入。

当前配置中不应再出现：

```yaml
Nats:
  Url: nats://ojos-nats:4222
```

也不应再出现：

```yaml
NATS_URL: nats://ojos-nats:4222
```

---

## 八、judge-api 配置结构

路径：

```text
services/judge-api/internal/config/config.go
```

当前推荐结构：

```go
package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Redis    RedisConfig
    Jaeger   JaegerConfig
}

type DatabaseConfig struct {
    Url string
}

type RedisConfig struct {
    Url string
}

type JaegerConfig struct {
    Endpoint string
}
```

如果当前暂时没有 `Jaeger`，可以先保留：

```go
type Config struct {
    rest.RestConf

    Database DatabaseConfig
    Redis    RedisConfig
}
```

但不应再有：

```go
Nats NatsConfig
```

也不应再有：

```go
type NatsConfig struct {
    Url string
}
```

---

## 九、judge-api ServiceContext

路径：

```text
services/judge-api/internal/svc/servicecontext.go
```

当前 `ServiceContext` 应包含：

```go
type ServiceContext struct {
    Config config.Config

    DB    *pgxpool.Pool
    Repo  *repository.Repository
    Redis *redis.Client

    UserContextMiddleware rest.Middleware
}
```

初始化流程：

```text
context.Background()
    ↓
连接 PostgreSQL
    ↓
Ping PostgreSQL
    ↓
解析 Redis URL
    ↓
连接 Redis
    ↓
Ping Redis
    ↓
初始化 Repository
    ↓
初始化 UserContextMiddleware
    ↓
返回 ServiceContext
```

Redis 初始化推荐：

```go
redisOptions, err := redis.ParseURL(c.Redis.Url)
if err != nil {
    log.Fatalf("parse redis url failed: %v", err)
}

redisClient := redis.NewClient(redisOptions)

if err := redisClient.Ping(ctx).Err(); err != nil {
    log.Fatalf("ping redis failed: %v", err)
}
```

当前不应再出现：

```go
Nats *nats.Conn
```

也不应再出现：

```go
nc, err := nats.Connect(c.Nats.Url)
```

---

## 十、数据库设计

Judge MVP 当前使用四张表：

```text
problems
test_cases
submissions
submission_cases
```

这些表由：

```text
deploy/migrations/000002_judge_schema.up.sql
```

创建。

---

### 10.1 problems 表

`problems` 当前是 MVP 题目表。

核心字段：

```text
id
title
time_limit_ms
memory_limit_mb
created_at
updated_at
```

字段说明：

| 字段                | 含义         |
| ----------------- | ---------- |
| `id`              | 题目 ID      |
| `title`           | 题目标题       |
| `time_limit_ms`   | 时间限制，单位毫秒  |
| `memory_limit_mb` | 内存限制，单位 MB |
| `created_at`      | 创建时间       |
| `updated_at`      | 更新时间       |

当前：

```text
time_limit_ms
```

已用于运行超时控制。

当前：

```text
memory_limit_mb
```

已存储，但尚未真实用于限制用户程序内存。

后续 Problem Core 正规化后，`problems` 表可能会被拆分或迁移为：

```text
problems
problem_versions
problem_statements
problem_settings
problem_visibility
problem_tags
problem_owners
```

当前 MVP 表不应继续无限扩展。

---

### 10.2 test_cases 表

`test_cases` 当前是 MVP 测试点表。

核心字段：

```text
id
problem_id
input
output
score
created_at
```

字段说明：

| 字段           | 含义      |
| ------------ | ------- |
| `id`         | 测试点 ID  |
| `problem_id` | 所属题目 ID |
| `input`      | 标准输入    |
| `output`     | 标准输出    |
| `score`      | 分值      |
| `created_at` | 创建时间    |

当前测试点输入输出直接存在数据库 TEXT 字段中。

这适合：

```text
MVP
小样例
本地功能验证
简单传统题
```

但不适合：

```text
大测试数据
多文件测试
交互题
通信题
提交答案题
压缩数据包
数据版本管理
数据权限管理
```

后续应迁移到：

```text
Dataset Core
文件化测试数据
对象存储或本地数据目录
测试数据 manifest
测试点组 / 子任务 / 捆绑点结构
```

---

### 10.3 submissions 表

`submissions` 用于存储提交记录和总评测结果。

核心字段：

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

字段说明：

| 字段           | 含义            |
| ------------ | ------------- |
| `id`         | 提交 ID         |
| `problem_id` | 题目 ID         |
| `user_id`    | 提交用户 ID       |
| `language`   | 语言，例如 `cpp17` |
| `code`       | 用户提交代码        |
| `status`     | 总评测状态         |
| `score`      | 总分            |
| `time_ms`    | 最大测试点耗时       |
| `memory_kb`  | 内存占用，当前暂未真实统计 |
| `message`    | 错误信息或系统信息     |
| `created_at` | 创建时间          |
| `updated_at` | 更新时间          |

当前 `user_id` 必须来自 Gateway 注入的可信用户上下文。

不应再从请求体中读取或信任：

```text
user_id
```

旧请求：

```json
{
  "problem_id": 1,
  "user_id": 2,
  "language": "cpp17",
  "code": "..."
}
```

已不推荐。

当前请求：

```json
{
  "problem_id": 1,
  "language": "cpp17",
  "code": "..."
}
```

---

### 10.4 submission_cases 表

`submission_cases` 用于存储每个测试点的评测结果。

核心字段：

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

字段说明：

| 字段              | 含义            |
| --------------- | ------------- |
| `submission_id` | 提交 ID         |
| `test_case_id`  | 测试点 ID        |
| `status`        | 测试点状态         |
| `time_ms`       | 该测试点运行时间      |
| `memory_kb`     | 内存占用，当前暂未真实统计 |
| `message`       | 错误信息          |
| `created_at`    | 创建时间          |

该表用于支持：

```http
GET /judge/submissions/:id/cases
```

用于查看每个测试点结果。

---

## 十一、评测状态设计

当前支持状态：

```text
PENDING
RUNNING
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
```

含义：

| 状态                     | 含义     |
| ---------------------- | ------ |
| `PENDING`              | 等待评测   |
| `RUNNING`              | 正在评测   |
| `ACCEPTED`             | 通过     |
| `WRONG_ANSWER`         | 答案错误   |
| `COMPILE_ERROR`        | 编译错误   |
| `RUNTIME_ERROR`        | 运行时错误  |
| `TIME_LIMIT_EXCEEDED`  | 超时     |
| `SYSTEM_ERROR`         | 系统错误   |
| `UNSUPPORTED_LANGUAGE` | 不支持的语言 |

当前状态流转：

```text
PENDING
    ↓
RUNNING
    ↓
ACCEPTED / WRONG_ANSWER / COMPILE_ERROR / RUNTIME_ERROR / TIME_LIMIT_EXCEEDED / SYSTEM_ERROR / UNSUPPORTED_LANGUAGE
```

当前通过数据库原子更新保证：

```text
只有 PENDING 可以被 claim 成 RUNNING
```

核心 SQL：

```sql
UPDATE submissions
SET status = 'RUNNING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

如果返回 1 行：

```text
当前 worker 抢到任务
```

如果返回 0 行：

```text
任务已经被其他 worker 抢走
或任务已经判完
或任务状态不是 PENDING
```

当前 worker 会跳过该任务。

---

## 十二、创建提交链路

当前创建提交的完整链路如下：

```text
Client
    ↓
POST /api/judge/submissions
    ↓
Gateway
    ↓
JWT 验证
    ↓
注入 X-User-Id / X-Username / X-Roles
    ↓
judge-api
    ↓
UserContextMiddleware 读取用户上下文
    ↓
CreateSubmissionLogic
    ↓
RequireUserPermission(user_id, judge.submit, system:0)
    ↓
Repo.CreateSubmission
    ↓
INSERT submissions(status=PENDING)
    ↓
Redis XADD ojos:judge:submissions
    ↓
返回 submission_id + PENDING
```

该链路保证：

```text
用户必须登录
用户身份来自 JWT
用户不能伪造 user_id
用户必须拥有 judge.submit
提交创建后有可靠队列任务
```

---

### 12.1 请求体

请求：

```http
POST /judge/submissions
```

经 Gateway：

```http
POST /api/judge/submissions
```

请求体：

```json
{
  "problem_id": 1,
  "language": "cpp17",
  "code": "#include <bits/stdc++.h>\nusing namespace std;\nint main(){int a,b;cin>>a>>b;cout<<a+b<<endl;}"
}
```

字段说明：

| 字段           | 说明            |
| ------------ | ------------- |
| `problem_id` | 题目 ID         |
| `language`   | 语言，例如 `cpp17` |
| `code`       | 用户代码          |

如果 `language` 为空，当前可以默认：

```text
cpp17
```

如果 `code` 为空，应拒绝。

如果 `problem_id <= 0`，应拒绝。

---

### 12.2 响应

响应示例：

```json
{
  "submission_id": 16,
  "status": "PENDING"
}
```

如果权限不足，当前可能返回普通错误：

```text
forbidden
```

后续统一错误响应后应改为：

```json
{
  "code": 40301,
  "msg": "forbidden"
}
```

---

### 12.3 权限检查

创建提交前必须检查：

```text
judge.submit @ system:0
```

Go 逻辑示例：

```go
user, ok := authctx.FromContext(l.ctx)
if !ok || user == nil || user.UserID <= 0 {
    return nil, errors.New("unauthorized")
}

if err := permission.RequireUserPermission(
    l.ctx,
    l.svcCtx.DB,
    user.UserID,
    "judge.submit",
    permission.SystemScope(),
); err != nil {
    return nil, err
}
```

权限检查必须发生在：

```text
写入 submission 之前
```

否则无权限用户也会制造垃圾提交。

---

## 十三、Redis Streams Judge Queue

当前 Judge Queue 使用 Redis Streams。

Stream：

```text
ojos:judge:submissions
```

Consumer Group：

```text
judge-workers
```

生产者：

```text
judge-api
```

消费者：

```text
judge-worker
```

消息类型：

```text
submission.created
```

消息字段：

```text
type
producer
submission_id
created_at
```

示例：

```text
type          submission.created
producer      judge-api-service
submission_id 16
created_at    2026-05-31T23:39:20Z
```

---

### 13.1 judge-api XADD

创建提交后，judge-api 执行：

```go
const judgeSubmissionStream = "ojos:judge:submissions"

func (l *CreateSubmissionLogic) publishSubmissionCreated(submissionID int64) error {
    return l.svcCtx.Redis.XAdd(
        l.ctx,
        &redis.XAddArgs{
            Stream: judgeSubmissionStream,
            Values: map[string]any{
                "type":          "submission.created",
                "producer":      "judge-api-service",
                "submission_id": strconv.FormatInt(submissionID, 10),
                "created_at":    time.Now().UTC().Format(time.RFC3339Nano),
            },
        },
    ).Err()
}
```

注意：

```text
XADD 失败时，submission 已经写入数据库
```

因此 worker 的 PENDING 扫描仍然必须保留。

---

### 13.2 为什么不用 Redis Pub/Sub

Redis Pub/Sub 和 NATS Core 类似，都是在线广播模型。

问题：

```text
worker 不在线时消息会丢
没有 pending list
没有 ACK
没有 consumer group
不适合可靠判题任务
```

Judge 任务不是普通通知，而是必须最终处理的任务。

因此当前使用：

```text
Redis Streams
```

而不是：

```text
Redis Pub/Sub
```

---

### 13.3 为什么保留 PENDING 扫描

即使使用 Redis Streams，也必须保留数据库 PENDING 扫描。

原因：

```text
judge-api 可能 INSERT submission 成功但 XADD 失败
Redis 可能短暂不可用
worker 可能消费后崩溃
worker 可能判题中途退出
旧版本可能遗留 PENDING
手动修复数据后需要恢复
```

所以可靠模型必须是：

```text
PostgreSQL 是最终事实来源
Redis Streams 是实时任务队列
PENDING 扫描是兜底恢复机制
try_claim_submission 是防重复执行机制
```

---

### 13.4 XACK 的含义

worker 判题结束后执行：

```text
XACK ojos:judge:submissions judge-workers <message_id>
```

含义：

```text
从 consumer group 的 Pending Entries List 中确认该消息
```

注意：

```text
XACK 不会删除 Stream 历史消息
```

因此：

```text
XRANGE ojos:judge:submissions - +
```

仍然能看到历史消息，这是正常的。

如果要限制 Stream 长度，后续可以使用：

```text
XTRIM ojos:judge:submissions MAXLEN ~ 10000
```

当前不用急。

---

## 十四、judge-worker 消费链路

worker 的完整消费链路：

```text
启动
    ↓
加载 languages.yaml
    ↓
连接 PostgreSQL
    ↓
连接 Redis
    ↓
XGROUP CREATE MKSTREAM
    ↓
启动扫描 PENDING
    ↓
启动定时扫描 PENDING
    ↓
循环 XREADGROUP
    ↓
解析 submission_id
    ↓
try_claim_submission
    ↓
编译运行
    ↓
写入结果
    ↓
XACK
```

---

### 14.1 Consumer Group 创建

worker 启动时应确保 group 存在：

```text
XGROUP CREATE ojos:judge:submissions judge-workers $ MKSTREAM
```

如果返回：

```text
BUSYGROUP
```

说明 group 已存在，正常继续。

---

### 14.2 XREADGROUP

worker 使用：

```text
XREADGROUP GROUP judge-workers <consumer-name>
COUNT 1
BLOCK 5000
STREAMS ojos:judge:submissions >
```

其中：

```text
>
```

表示读取从未投递给其他 consumer 的新消息。

如果 Redis 没有新消息，`BLOCK 5000` 可能超时。

这个超时不能导致 worker 退出。

正确行为：

```text
timeout
    ↓
continue loop
```

之前曾出现过：

```text
xreadgroup failed: timed out
worker 退出
```

已经修正为：

```text
timeout 继续循环
其他错误记录日志后 sleep 再继续
```

---

### 14.3 submission_id 解析

Redis Stream 消息中 `submission_id` 是字符串。

worker 需要从 message map 中读取：

```text
submission_id
```

并解析为 `i64`。

如果缺失或非法：

```text
记录 warn
XACK 消息
跳过
```

不要因为一条坏消息导致 worker 退出。

---

### 14.4 try_claim_submission

即使 worker 从 Redis 读到任务，也不能直接判题。

必须先执行数据库原子抢任务：

```sql
UPDATE submissions
SET status = 'RUNNING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

如果抢到：

```text
继续判题
```

如果没抢到：

```text
说明任务已经不是 PENDING
跳过并 ACK Redis 消息
```

这可以防止：

```text
Redis 重复投递
worker 启动扫描先判完
多个 worker 同时消费
旧消息滞留
```

---

## 十五、PENDING 兜底机制

当前 worker 有两类 PENDING 扫描。

### 15.1 启动扫描

worker 启动后立即扫描：

```sql
SELECT id
FROM submissions
WHERE status = 'PENDING'
ORDER BY id ASC
LIMIT $1;
```

这样可以恢复：

```text
worker 停机时创建的 submission
XADD 失败的 submission
旧版本遗留的 PENDING
```

---

### 15.2 定时扫描

worker 运行期间每隔一段时间扫描 PENDING。

当前建议周期：

```text
10 秒
```

如果发现 PENDING：

```text
逐个 handle_submission
```

如果没有发现 PENDING：

```text
不要频繁 info 打日志
```

避免日志刷屏。

---

### 15.3 扫描与 Redis 消息重复的处理

可能出现：

```text
启动扫描先处理 submission 15
随后 Redis Stream 又投递 submission 15
```

此时 Redis 消息到达后：

```text
try_claim_submission 返回 false
worker 记录 skip
XACK 消息
```

这是正常行为。

这说明系统防重复机制生效。

---

## 十六、编译与运行模型

当前 worker 评测流程：

```text
创建临时工作目录
    ↓
写入用户代码文件
    ↓
根据 languages.yaml 判断是否需要编译
    ↓
执行编译命令
    ↓
编译失败 -> COMPILE_ERROR
    ↓
读取测试点
    ↓
逐个运行程序
    ↓
传入 stdin
    ↓
收集 stdout / stderr / exit status / time
    ↓
比较 stdout 与标准输出
    ↓
写入 submission_cases
    ↓
汇总 submissions
```

当前比较方式：

```text
标准输出文本比较
```

通常应做：

```text
去除末尾空白差异
按行或整体 trim
```

当前如果只是简单 trim 后比较，可以满足 MVP。

后续需要抽象：

```text
checker-standard
checker-special
checker-float
checker-interactive
checker-output-only
```

---

## 十七、languages.yaml

路径：

```text
services/judge-worker/config/languages.yaml
```

该文件定义不同语言的：

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

```text
{source}
{exe}
{workdir}
```

当前可支持语言取决于 Docker 镜像中是否安装相应工具链。

推荐支持：

```text
cpp17
cpp20
c11
python3
java17
go
rust
```

语言配置原则：

```text
语言命令不硬编码在 Rust 代码中
新增语言优先修改 languages.yaml
语言包后续可以模块化
编译超时和运行超时分开
```

---

## 十八、时间与内存

### 18.1 time_ms

当前 `time_ms` 记录程序运行时间。

如果结果为：

```text
0ms
```

这是正常的。

在算法竞赛中，极小程序运行时间显示为 0ms 很常见，不需要强行改成 1ms。

---

### 18.2 memory_kb

当前 `memory_kb` 尚未真实统计。

可能显示：

```text
0
```

这是 MVP 阶段可接受的已知限制。

后续 Runner Core / Sandbox 完成后，应从运行器收集真实内存峰值，例如：

```text
cgroup memory.max_usage_in_bytes
rusage
sandbox report
```

当前不要伪造内存数据。

---

## 十九、权限接入

当前 Judge API 已接入 Permission Core。

当前检查点：

```text
POST /judge/submissions
    -> judge.submit @ system:0
```

也就是说，用户创建提交前必须拥有：

```text
judge.submit
```

当前普通 `user` 角色拥有该权限。

如果插入直接 deny：

```text
deny judge.submit @ system:0
```

该用户提交会被拒绝。

权限测试 SQL：

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

删除 deny：

```sql
DELETE FROM permission_assignments
WHERE principal_type = 'user'
  AND principal_id = (SELECT id FROM users WHERE username = 'permtest')
  AND permission_code = 'judge.submit'
  AND scope_type = 'system'
  AND scope_id = 0;
```

后续 Judge 相关权限还应扩展：

```text
submission.view.own
submission.view.all
submission.rejudge
submission.delete
```

当前查询提交接口是否需要权限隔离，后续应补。

---

## 二十、API 文档

### 20.1 创建题目 MVP

请求：

```http
POST /judge/problems
```

Gateway：

```http
POST /api/judge/problems
```

请求体示例：

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

说明：

```text
这是 MVP 接口
后续应迁移到 problem-api
```

当前如果该接口尚未接入权限检查，后续应至少要求：

```text
problem.create @ system:0
```

---

### 20.2 添加测试点 MVP

请求：

```http
POST /judge/test-cases
```

Gateway：

```http
POST /api/judge/test-cases
```

请求体示例：

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

说明：

```text
这是 MVP 接口
后续应迁移到 dataset-core / problem-api
```

后续应要求：

```text
problem.manage.data @ problem:{id}
```

---

### 20.3 创建提交

请求：

```http
POST /judge/submissions
```

Gateway：

```http
POST /api/judge/submissions
```

请求体示例：

```json
{
  "problem_id": 1,
  "language": "cpp17",
  "code": "#include <bits/stdc++.h>\nusing namespace std;\nint main(){int a,b;cin>>a>>b;cout<<a+b<<endl;}"
}
```

响应示例：

```json
{
  "submission_id": 16,
  "status": "PENDING"
}
```

创建后：

```text
submissions.status = PENDING
Redis Stream 写入 submission.created
```

---

### 20.4 查询提交

请求：

```http
GET /judge/submissions/:id
```

Gateway：

```http
GET /api/judge/submissions/:id
```

响应示例：

```json
{
  "id": 16,
  "problem_id": 1,
  "user_id": 2,
  "language": "cpp17",
  "status": "ACCEPTED",
  "score": 100,
  "time_ms": 0,
  "memory_kb": 0,
  "message": ""
}
```

后续应补权限：

```text
如果是自己的提交 -> submission.view.own
如果是别人的提交 -> submission.view.all
比赛中还要考虑封榜和反馈策略
```

当前 MVP 可能还没有完整隔离，需要后续修。

---

### 20.5 查询测试点结果

请求：

```http
GET /judge/submissions/:id/cases
```

Gateway：

```http
GET /api/judge/submissions/:id/cases
```

响应示例：

```json
{
  "cases": [
    {
      "id": 1,
      "submission_id": 16,
      "test_case_id": 1,
      "status": "ACCEPTED",
      "time_ms": 0,
      "memory_kb": 0,
      "message": ""
    }
  ]
}
```

后续比赛环境下，该接口要受反馈策略影响。

例如：

```text
ACM 赛时可能不显示详细测试点
OI 赛时可能显示分数但不显示数据点详情
IOI 可能有更复杂反馈策略
封榜后可能限制部分信息
```

当前 MVP 直接展示测试点结果，后续要改。

---

## 二十一、验收命令

### 21.1 登录

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

如果响应是直接 token：

```powershell
$token = $res.token
```

---

### 21.2 提交 AC 代码

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

### 21.3 查看 worker 日志

```powershell
docker logs ojos-judge-worker --tail 100
```

预期看到：

```text
received judge stream message
submission claimed
start judging
judge finished
judge stream message acked
```

---

### 21.4 查询提交结果

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
```

---

### 21.5 查询测试点

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/$($res.submission_id)/cases" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期：

```text
cases[0].status = ACCEPTED
```

---

### 21.6 检查 Redis Stream

```powershell
docker exec -it ojos-redis redis-cli XINFO STREAM ojos:judge:submissions
docker exec -it ojos-redis redis-cli XINFO GROUPS ojos:judge:submissions
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

预期：

```text
XPENDING = 0
```

---

## 二十二、NATS 清理检查

Judge 模块当前不应再依赖 NATS。

检查 judge-api：

```powershell
cd D:\Untitled-OJ\services\judge-api

Get-ChildItem -Recurse -Include *.go,*.yaml,go.mod,go.sum |
  Select-String -Pattern "nats|NATS|Nats|4222|github.com/nats-io"
```

预期无输出。

检查 judge-worker：

```powershell
cd D:\Untitled-OJ\services\judge-worker

Get-ChildItem -Recurse -Include *.rs,Cargo.toml,Cargo.lock |
  Select-String -Pattern "nats|async_nats|async-nats|4222"
```

预期无输出。

全项目检查：

```powershell
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

注意：

```text
event-listener 不是 NATS
zipkin 不是 NATS
```

不要误删。

---

## 二十三、编译与运行

### 23.1 judge-api 编译

```powershell
cd D:\Untitled-OJ\services\judge-api

go mod tidy
go build .
```

### 23.2 judge-worker 编译

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

### 23.3 Docker 重建

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build judge-api judge-worker
```

### 23.4 查看日志

```powershell
docker logs ojos-judge-api --tail 100
docker logs ojos-judge-worker --tail 100
```

---

## 二十四、常见问题

### 24.1 提交一直 PENDING

排查顺序：

```text
1. judge-api 是否成功 XADD Redis Stream
2. Redis Stream 是否有消息
3. judge-worker 是否运行
4. judge-worker 是否连接 Redis
5. judge-worker 是否 XREADGROUP 超时后退出
6. worker 是否启动扫描 PENDING
7. try_claim_submission 是否成功
8. worker 是否编译或运行失败
```

检查 Redis：

```powershell
docker exec -it ojos-redis redis-cli XRANGE ojos:judge:submissions - +
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

检查数据库：

```sql
SELECT id, status, message, updated_at
FROM submissions
ORDER BY id DESC
LIMIT 10;
```

---

### 24.2 worker 启动后马上退出

如果日志出现：

```text
xreadgroup failed: timed out
```

说明 Redis read timeout 被当成 fatal error。

正确处理：

```text
timeout -> continue
其他错误 -> log error + sleep + continue
```

worker 不应该因为没有新消息而退出。

---

### 24.3 submission skipped because it is not pending

这不是错误。

原因可能是：

```text
启动扫描已经判完该 submission
Redis Stream 中仍有对应历史消息
worker 再次收到后 try_claim_submission 失败
于是跳过并 ACK
```

这是防重复判题机制正常工作。

---

### 24.4 XPENDING 不为 0

说明有消息被投递给 consumer 但未 ACK。

排查：

```text
worker 是否崩溃
worker 是否在判题中卡住
worker 是否处理完后没有 XACK
Redis 连接是否异常
```

后续可以加入：

```text
XAUTOCLAIM
```

处理长时间 pending 的 stream 消息。

当前即使 Redis PEL 中有残留，数据库 PENDING 扫描仍可兜底。

---

### 24.5 0ms 是否异常

不是异常。

极小程序运行时间显示为 0ms 在算法竞赛系统中很正常。

不要强行改成 1ms。

---

### 24.6 memory_kb 为 0

当前是已知限制。

原因：

```text
还没有 sandbox / cgroup 统计真实内存
```

后续 Runner Core 完成后再处理。

---

### 24.7 直接访问 judge-api 失败

如果直接访问：

```text
http://localhost:8082/judge/submissions
```

可能失败，因为没有 Gateway 注入的用户 Header。

正常测试应通过：

```text
http://localhost:8080/api/judge/submissions
```

除非手动注入：

```text
X-Auth-Verified
X-User-Id
X-Username
X-Roles
```

但这只用于内部调试，不应用于真实访问。

---

## 二十五、安全限制

当前最大问题是：

```text
用户代码直接运行在 judge-worker 容器内
```

这意味着恶意代码可能：

```text
读取容器内文件
占用 CPU
占用内存
创建大量进程
访问网络
影响 worker 进程
影响其他评测任务
```

当前必须尽快规划：

```text
Runner Core
Sandbox Provider
容器隔离
network none
CPU 限制
memory 限制
pids 限制
文件系统隔离
临时目录隔离
超时杀进程
```

在安全隔离完成前，不应对外开放真实不可信用户评测。

当前系统适合：

```text
本地开发
功能验证
可信环境内测试
OJOS 架构推进
```

不适合：

```text
公网开放
承接正式比赛
执行陌生用户代码
```

---

## 二十六、后续拆分方向

当前 `judge-api` 同时包含：

```text
题目 MVP
测试点 MVP
提交
查询
任务投递
```

后续应拆分为：

```text
problem-api
dataset-core
judge-api
judge-worker
runner-core
checker-core
scoring-core
```

---

### 26.1 problem-api

负责：

```text
题目基本信息
题面
标签
难度
可见性
题目权限
题目类型
题目版本
```

替代当前：

```text
POST /judge/problems
```

---

### 26.2 dataset-core

负责：

```text
测试数据文件化
测试点列表
子任务
捆绑点
样例
数据包
数据权限
数据校验
```

替代当前：

```text
POST /judge/test-cases
```

---

### 26.3 judge-api

最终只负责：

```text
创建提交
查询提交
查询测试点结果
重测任务
提交状态
```

---

### 26.4 runner-core

负责：

```text
编译
运行
资源限制
进程隔离
网络隔离
文件系统隔离
收集原始运行结果
```

---

### 26.5 checker-core

负责：

```text
标准输出比较
Special Judge
浮点误差
交互 checker
提交答案 checker
```

---

### 26.6 scoring-core

负责：

```text
ACM 得分
OI 子任务得分
NOI 捆绑点
IOI 反馈策略
启发式题评分
```

---

## 二十七、下一阶段建议

Judge 模块下一阶段不建议直接上复杂赛制。

推荐顺序：

```text
1. 统一错误响应
2. problem-api / dataset-core 正规化
3. judge-api 移除 problem / test-case 管理职责
4. Runner Core 抽象
5. 基础安全隔离
6. 测试数据文件化
7. checker-standard 抽象
8. Special Judge
9. 子任务 / 捆绑点
10. problem-type-traditional
11. contest-core
12. contest-rule-acm
13. scoreboard-acm
```

当前最关键的是：

```text
题目和测试数据模型稳定
执行安全模型稳定
评测结果结构稳定
```

没有这些，直接做 contest-core 和 scoreboard 会不断返工。

---

## 二十八、当前结论

Judge 模块当前已经完成 OJOS 的核心闭环：

```text
登录
鉴权
提交
排队
消费
判题
回写
查询
```

当前 Judge 链路已经从不可靠的：

```text
NATS Core Pub/Sub
```

升级为：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
```

这使 Judge 任务具备了更好的可靠性。

当前 Judge 模块已经可以作为后续 OJOS 开发的基础，但还不是生产级安全 Judge。

后续必须重点推进：

```text
Problem Core
Dataset Core
Runner Core
安全沙箱
Checker 抽象
Scoring 抽象
```

只有这些稳定后，才能可靠支持：

```text
OI
NOI
ACM
IOI
启发式算法题
子任务
捆绑点
交互题
通信题
提交答案题
滚榜
封榜
气球
打印
ICPC Tools 兼容
```
