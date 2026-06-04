# OJOS 架构总览

## 一、项目整体定位

OJOS，全称：

```text
Online Judge Operating System
```

它不是传统意义上的单体 Online Judge 网站，而是一个面向长期扩展的 OJ 基础设施平台。

传统 OJ 通常关注：

```text
用户注册
题目列表
提交代码
返回结果
比赛榜单
```

OJOS 的目标更大。它希望成为一个可以持续扩展、可以安装模块、可以支持多赛制、多题型、多运行器、多权限范围、多比赛运营能力的 Online Judge 基础平台。

OJOS 的长期目标包括：

```text
支持 OI / NOI / ACM / IOI / 启发式算法题
支持传统题 / 交互题 / 通信题 / 提交答案题
支持子任务 / 捆绑点 / 部分分 / 多反馈策略
支持滚榜 / 封榜 / ICPC Tools 兼容格式
支持赛时发气球
支持打印代码 / 打印小票
支持题库权限
支持比赛组建
支持资源级权限
支持帖子 / 公告 / Clarification
支持模块开启 / 关闭
支持可视化模块安装器
支持在前置模块满足时直接安装新模块
支持通过 PR 仓库新增模块
支持新增模块不修改其他模块即可接入
```

因此 OJOS 的核心不是“写一个 OJ 页面”，而是构建一套：

```text
OJ Infrastructure Platform
```

也就是：

```text
OJOS Kernel + Modules
```

其中：

```text
Kernel
```

负责认证、权限、网关、基础设施、可观测性、模块注册、功能开关等基础能力。

```text
Modules
```

负责题库、比赛、判题、榜单、赛制、题型、气球、打印、帖子、Clarification、训练等业务能力。

---

## 二、核心架构原则

### 2.1 Everything is a Module

OJOS 中的能力应尽量模块化。

例如：

```text
auth
gateway
permission-core
problem-core
dataset-core
judge-core
runner-core
checker-core
scorer-core
contest-core
scoreboard-core
balloon-service
print-service
forum-service
clarification-service
module-registry
launcher
```

都可以视为模块。

但是模块化不等于每个能力都必须单独一个 Docker 容器。

模块应该分为：

```text
服务级模块
能力级模块
资源级模块
配置级模块
前端级模块
```

例如：

```text
auth
gateway
problem-api
judge-api
judge-worker
contest-api
scoreboard-api
```

适合作为服务级模块。

而：

```text
contest-rule-acm
contest-rule-oi
contest-rule-ioi
problem-type-traditional
problem-type-interactive
traditional-runner
interactive-runner
default-trim-checker
special-checker
default-sum-scorer
ioi-scorer
```

更适合作为能力级模块，嵌入在对应核心服务内部。

这样可以避免把系统拆成几十个微服务，导致：

```text
本地开发困难
Docker Compose 复杂
日志排查困难
网络调用膨胀
数据库事务边界复杂
模块依赖混乱
```

---

### 2.2 Everything is Permission-Controlled

OJOS 的所有重要能力都应该进入权限系统。

权限判断统一交给：

```text
Permission Core
```

权限模型是：

```text
Can(principal, permission, scope)
```

例如：

```text
Can(user:1, "judge.submit", system:0)
Can(user:2, "problem.edit", problem:7)
Can(user:3, "problem.manage.data", problem:7)
Can(user:4, "contest.manage", contest:5)
Can(user:5, "scoreboard.roll", contest:5)
Can(user:6, "module.install", system:0)
```

系统不应该只靠简单角色判断：

```text
if role == admin
```

而应该使用完整资源级权限：

```text
principal_type
principal_id
permission_code
scope_type
scope_id
```

这使得 OJOS 可以支持：

```text
某人只能管理某道题
某人只能管理某场比赛
某人只能在某场比赛中发气球
某人只能操作打印服务
某人可以安装模块
某人可以查看封榜后的后台榜单
某人可以重测某个题目的全部提交
某人只能查看自己的提交
```

---

### 2.3 Gateway Handles Authentication, Services Handle Authorization

Gateway 负责认证入口。

也就是说 Gateway 负责：

```text
JWT 验证
用户上下文解析
可信 Header 注入
请求代理
trace context 转发
```

但 Gateway 不负责具体业务权限判断。

错误设计：

```text
Gateway 判断 user 是否能 problem.edit
Gateway 判断 user 是否能 contest.freeze
Gateway 判断 user 是否能 module.install
```

正确设计：

```text
Gateway 判断请求是否登录
业务服务判断具体权限
```

例如：

```text
POST /api/judge/submissions
    Gateway:
        检查 JWT
        注入可信用户上下文

    judge-api:
        检查 judge.submit @ system:0
```

又如：

```text
PUT /api/problem/problems/:id/test-cases/:case_no
    Gateway:
        检查 JWT
        注入可信用户上下文

    problem-api:
        检查 problem.manage.data @ problem:{id}
```

这样可以保证：

```text
新增模块不需要修改 Gateway 业务代码
Gateway 不耦合业务表
业务服务拥有最完整的业务上下文
Permission Core 统一负责授权判断
```

---

### 2.4 PostgreSQL is the Source of Truth

OJOS 当前使用 PostgreSQL 作为事实源。

所有关键业务状态最终必须落在 PostgreSQL 中，包括：

```text
用户
角色
权限
题目元数据
题目包路径
提交摘要
提交状态
取消记录
重测状态
比赛
榜单
模块安装状态
功能开关
审计日志
```

Redis 可以作为：

```text
队列
缓存
临时状态
限流计数器
```

但 Redis 不应成为不可恢复的唯一事实源。

以 Judge 为例：

```text
submissions.status
```

才是提交的最终状态。

Redis Streams 只负责实时任务投递。

即使 Redis 消息丢失或异常，PostgreSQL 中的：

```text
PENDING submission
```

仍然可以被 worker 扫描恢复。

---

### 2.5 File Storage Holds Large Judge Artifacts

OJOS 不把题目数据、源码正文、完整 case 结果直接塞进数据库。

当前文件化存储分为两类：

```text
storage/problems
storage/submissions
```

题目包放在：

```text
storage/problems/{id}-{slug}/
```

提交文件放在：

```text
storage/submissions/{submission_id}/
```

数据库只保存：

```text
题目元数据
题目包 package_dir
提交摘要
源码 code_path
源码 code_sha256
结果 result_path
```

这种设计避免：

```text
数据库 TEXT 字段存大测试数据
数据库存大量 stdout / stderr / checker log
重测时重复读写大字段
后续导入 Polygon / 导出题目包困难
```

---

### 2.6 Redis Streams for Reliable Tasks

当前 Judge 队列使用 Redis Streams。

原因是 Judge 任务不是普通事件通知，而是可靠任务。

Judge 任务需要：

```text
持久化
Consumer Group
ACK
Pending List
多 worker 竞争消费
失败恢复
积压查看
```

Redis Streams 能提供这些能力。

当前 Judge 队列：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

生产者：

```text
judge-api
```

消费者：

```text
judge-worker
```

当前链路：

```text
judge-api
    ↓
Redis Stream XADD
    ↓
judge-worker XREADGROUP
    ↓
try_claim_submission
    ↓
judge
    ↓
XACK
```

当前 OJOS 已经移除 NATS。

---

### 2.7 Sandbox First for Untrusted Code

Judge Worker 必须把用户代码当成不可信程序。

当前使用：

```text
nsjail
```

作为基础沙箱。

核心目标是：

```text
用户程序看不到题目答案
用户程序不能覆写题目数据
用户程序不能访问 worker 的完整文件系统
用户程序不能以 root 身份运行
用户程序不能访问外部网络
每个测试点独立运行
```

当前基本边界：

```text
用户程序 uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
每个 case 独立 workdir
题目 in / ans 不挂入用户程序可见路径
stdout / stderr / checker.log 写入 submission 目录
```

后续仍需继续完善：

```text
cgroup v2 memory peak 统计
更精细的 CPU 限制
输出大小限制
系统调用策略
多语言隔离策略
```

---

### 2.8 Do Not Over-Abstract Too Early

OJOS 的目标是模块化，但不能为了“看起来架构高级”提前抽象所有东西。

已经验证过的问题包括：

```text
过早封装 shared/events
过早绑定 NATS
过早把 config 放进 shared
过早做 response 包
```

这些会导致后续重构成本很高。

现在的原则是：

```text
重复三次再抽象
稳定之后再抽象
不确定的模块边界先保留在业务服务内部
公共能力进入 shared
业务能力留在业务服务
```

例如：

```text
Redis Streams 当前先在 judge-api / judge-worker 内部实现
等 print task / balloon task / rejudge task 都出现后，再抽象 shared/queue
```

而不是现在就写一个通用队列框架。

---

## 三、当前整体架构

当前实际架构为：

```text
Client / Frontend
        ↓
Gateway :8080
        ↓
 ┌───────────────┬─────────────────┬────────────────┐
 │               │                 │                │
Auth :8081   Problem API :8083  Judge API :8082   Future Services
 │               │                 │
 │               │                 ↓
 │               │          Redis Stream
 │               │       ojos:judge:submissions
 │               │                 ↓
 │               │          Judge Worker
 │               │                 ↓
 └───────────────┴────────── PostgreSQL ─────────────┘

File Storage:
    storage/problems
    storage/submissions

Observability:
    OpenTelemetry -> Jaeger

Cache / Queue:
    Redis

Removed:
    NATS
```

当前服务列表：

```text
gateway
auth
problem-api
judge-api
judge-worker
shared
```

当前基础设施：

```text
PostgreSQL
Redis
Jaeger
Docker Compose
```

当前已删除基础设施：

```text
NATS
```

---

## 四、当前服务职责

### 4.1 Frontend

路径：

```text
frontend
```

当前前端基于 Vite / Vue。

前端未来职责：

```text
登录 / 注册
题目列表
题目管理
提交代码
查看提交结果
比赛页面
榜单页面
权限管理页面
模块安装页面
后台管理页面
```

当前前端不是架构核心，后续在后端稳定后再系统推进。

---

### 4.2 Gateway

路径：

```text
services/gateway
```

Gateway 是统一 HTTP 入口。

当前职责：

```text
监听 8080
提供 /health
代理 /api/auth/*
代理 /api/problem/*
代理 /api/judge/*
JWT 验证
AuthMode 判断
可信用户上下文注入
清理伪造 Header
trace context 传播
基础日志
panic recovery
```

Gateway 不负责：

```text
资源级权限
题目业务
比赛业务
判题业务
榜单业务
模块安装业务
```

---

### 4.3 Auth

路径：

```text
services/auth
```

Auth 是认证服务。

当前职责：

```text
注册用户
登录用户
bcrypt 密码哈希
JWT 签发
Profile 查询
默认 user 角色绑定
```

Auth 不负责：

```text
judge.submit 判断
problem.edit 判断
contest.manage 判断
module.install 判断
```

这些由 Permission Core 判断。

---

### 4.4 Shared

路径：

```text
services/shared
```

Shared 是 Go 公共基础库，不是服务。

当前职责：

```text
database
logger
middleware
tracing
security/jwt
security/authctx
security/permission
```

Shared 不负责：

```text
业务配置
业务逻辑
HTTP 路由
服务生命周期
```

---

### 4.5 Permission Core

路径：

```text
services/shared/security/permission
```

Permission Core 是完整资源级权限系统。

当前职责：

```text
权限主体
资源作用域
权限点
角色
直接授权
直接拒绝
资源继承
权限审计
权限检查
```

当前已真实接入：

```text
judge-api POST /judge/submissions
    -> judge.submit @ system:0

judge-api POST /judge/submissions/:id/cancel
    -> problem.manage.data @ problem:{id}

judge-api POST /judge/problems/:id/rejudge
    -> problem.manage.data @ problem:{id}
```

---

### 4.6 Problem API

路径：

```text
services/problem-api
```

Problem API 是题目与题目包管理服务。

当前职责：

```text
创建题目
查询题目
更新题目
删除题目
维护 problems 表
维护 problems.package_dir
创建题目包目录
写入 problem.yaml
写入 statement
写入 tutorial
写入 tests/cases.yaml
写入 tests/groups.yaml
写入 checker / runner / scorer 配置
管理测试点文件
```

Problem API 不负责：

```text
创建提交
运行代码
比较输出
写入 submission 结果
```

这些由 Judge API 和 Judge Worker 负责。

---

### 4.7 Judge API

路径：

```text
services/judge-api
```

Judge API 是判题 HTTP API 层。

当前职责：

```text
创建提交
查询提交摘要
查询提交 case 结果
取消单份提交成绩
重测某题全部提交
读取用户上下文
检查 judge.submit
检查 problem.manage.data
写入 submissions
写入 storage/submissions/{id}/source
写入 result.json 初始文件
Redis XADD 投递任务
```

Judge API 不负责：

```text
创建题目
添加测试点
管理题面
管理题目数据
执行代码
比较输出
写入每个 case 的数据库记录
```

以下旧职责已经迁移或废弃：

```text
POST /judge/problems
POST /judge/test-cases
test_cases
submission_cases
submissions.code
```

---

### 4.8 Judge Worker

路径：

```text
services/judge-worker
```

Judge Worker 是 Rust 后台判题进程。

当前职责：

```text
连接 PostgreSQL
连接 Redis
加载 languages.yaml
确保 Redis Consumer Group
启动扫描 PENDING
定时扫描 PENDING
XREADGROUP 消费任务
try_claim_submission
读取 submissions.code_path
读取 problems.package_dir
读取 problem.yaml
读取 tests/cases.yaml
nsjail 编译用户代码
nsjail 逐 case 运行用户代码
default-trim-checker
default-sum-scorer
写入 stdout.txt / stderr.txt / checker.log
写入 result.json
更新 submissions 摘要
XACK 消息
```

当前 Judge Worker 不提供 HTTP 接口。

---

## 五、核心数据流

### 5.1 用户登录

```text
Client
    ↓
POST /api/auth/login
    ↓
Gateway
    ↓
Auth
    ↓
查询 users
    ↓
bcrypt 校验密码
    ↓
查询 roles
    ↓
生成 JWT
    ↓
返回 token
```

关键点：

```text
Auth 签发 JWT
Gateway 后续解析 JWT
Jwt.Secret 必须一致
```

---

### 5.2 用户访问受保护接口

```text
Client
    ↓
Authorization: Bearer <token>
    ↓
Gateway
    ↓
解析 JWT
    ↓
清理伪造 Header
    ↓
注入 X-Auth-Verified / X-User-Id / X-Username / X-Roles
    ↓
代理到业务服务
```

业务服务通过：

```text
UserContextMiddleware
```

读取可信 Header，并写入 context。

---

### 5.3 创建题目

```text
Client
    ↓
POST /api/problem/problems
    ↓
Gateway
    ↓
problem-api
    ↓
检查 problem.create 权限
    ↓
INSERT problems
    ↓
创建 storage/problems/{id}-{slug}
    ↓
写入 problem.yaml
    ↓
写入默认 runner/checker/scorer
    ↓
写入 statement / tutorial / tests
```

其中：

```text
problems.package_dir
```

指向题目包目录，是 Judge Worker 后续读取题目数据的入口。

---

### 5.4 用户提交代码

```text
Client
    ↓
POST /api/judge/submissions
    ↓
Gateway required auth
    ↓
judge-api
    ↓
authctx.FromContext
    ↓
Permission Core: judge.submit @ system:0
    ↓
读取 problem 元数据和 package_dir
    ↓
INSERT submissions(status=PENDING)
    ↓
写入 storage/submissions/{id}/source/main.cpp
    ↓
写入 storage/submissions/{id}/result.json 初始内容
    ↓
Redis XADD ojos:judge:submissions
    ↓
返回 submission_id
```

---

### 5.5 Worker 判题

```text
judge-worker
    ↓
XREADGROUP ojos:judge:submissions
    ↓
解析 submission_id
    ↓
UPDATE submissions
        SET status='JUDGING'
        WHERE id=? AND status='PENDING'
        RETURNING id
    ↓
load submission
    ↓
load problem.package_dir
    ↓
load problem.yaml
    ↓
load tests/cases.yaml
    ↓
nsjail compile
    ↓
for each case:
        copy input -> submission case workdir
        nsjail run
        compare stdout with answer
        write stdout.txt / stderr.txt / checker.log
    ↓
write result.json
    ↓
UPDATE submissions final status
    ↓
XACK
```

---

### 5.6 查询提交结果

```text
Client
    ↓
GET /api/judge/submissions/:id
    ↓
Gateway
    ↓
judge-api
    ↓
SELECT submissions
    ↓
返回 status / score / time / memory / message / paths
```

完整 case 结果由：

```text
result_path
```

指向的 `result.json` 提供。

---

### 5.7 查询提交 Case 结果

```text
Client
    ↓
GET /api/judge/submissions/:id/cases
    ↓
Gateway
    ↓
judge-api
    ↓
读取 submissions.result_path
    ↓
读取 result.json
    ↓
返回 cases 数组
```

这意味着：

```text
数据库不再保存 submission_cases
```

---

### 5.8 Cancel 单份提交

```text
Client
    ↓
POST /api/judge/submissions/:id/cancel
    ↓
Gateway
    ↓
judge-api
    ↓
读取 submission 和 problem
    ↓
检查 problem.manage.data @ problem:{problem_id}
    ↓
UPDATE submissions
        SET status='CANCELLED',
            cancelled_at=NOW(),
            cancelled_by=<current_user>,
            cancel_reason=<reason>
```

Cancel 的语义是：

```text
取消当前这份提交的成绩
```

不是删除提交。

---

### 5.9 Rejudge 某题全部提交

```text
Client
    ↓
POST /api/judge/problems/:id/rejudge
    ↓
Gateway
    ↓
judge-api
    ↓
检查 problem.manage.data @ problem:{id}
    ↓
SELECT all submissions WHERE problem_id = id
    ↓
UPDATE submissions
        SET status='PENDING',
            score=0,
            time_ms=0,
            memory_kb=0,
            message='',
            judged_at=NULL,
            cancelled_at=NULL,
            cancelled_by=NULL,
            cancel_reason=''
    ↓
XADD 每个 submission_id 到 Redis Stream
```

Rejudge 的语义是：

```text
重测该题全部提交，包括 CANCELLED
```

因此 rejudge 会覆盖 cancel 状态。

---

## 六、文件存储架构

### 6.1 题目包目录

题目包位于：

```text
storage/problems/{id}-{slug}/
```

示例：

```text
storage/problems/2-a-plus-b/
```

推荐结构：

```text
storage/problems/{id}-{slug}/

├── problem.yaml
├── statement/
│   ├── zh-cn.md
│   └── assets/
├── tests/
│   ├── cases.yaml
│   ├── groups.yaml
│   ├── 001.in
│   └── 001.ans
├── checker/
│   └── checker.yaml
├── runner/
│   └── runner.yaml
├── scorer/
│   └── scorer.yaml
└── tutorial/
    ├── zh-cn.md
    └── std.cpp
```

核心约定：

```text
problem.yaml 是题目包入口
tests.cases 是相对 package_dir 的路径，例如 tests/cases.yaml
tests.root 是测试数据根目录，例如 tests
case.input / case.answer 是相对 tests.root 的路径
case_no 从 1 开始
不使用 no: 0
```

---

### 6.2 提交目录

提交文件位于：

```text
storage/submissions/{submission_id}/
```

示例：

```text
storage/submissions/20/
```

推荐结构：

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

说明：

```text
source/      保存用户源码
build/       保存编译产物和编译日志
cases/       保存每个测试点的运行输入输出和 checker 日志
result.json  保存完整评测结果
```

数据库只存摘要和路径。

---

## 七、数据库架构

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
submissions

schema_migrations
```

当前已经删除或废弃：

```text
test_cases
submission_cases
submissions.code
```

---

### 7.1 Auth 表

```text
users
roles
user_roles
```

用于：

```text
用户身份
密码哈希
基础角色
```

---

### 7.2 Permission 表

```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

用于：

```text
资源级权限
角色权限
直接 allow / deny
资源继承
审计日志
```

---

### 7.3 Problem 表

`problems` 用于保存题目元数据和题目包入口。

重点字段：

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

其中：

```text
package_dir
```

是 Judge Worker 读取题目包的入口。

---

### 7.4 Submission 表

`submissions` 用于保存提交摘要和评测状态。

重点字段：

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

其中：

```text
code_path
```

指向：

```text
storage/submissions/{id}/source/*
```

`result_path` 指向：

```text
storage/submissions/{id}/result.json
```

当前：

```text
memory_kb
```

暂为 0，后续通过 cgroup v2 统计峰值内存。

---

## 八、基础设施架构

### 8.1 PostgreSQL

用途：

```text
事实源
用户数据
权限数据
题目元数据
提交状态
迁移状态
```

容器内连接：

```text
postgres://postgres:password@postgres:5432/ojos?sslmode=disable
```

宿主机迁移连接：

```text
postgres://postgres:password@localhost:5433/ojos?sslmode=disable
```

---

### 8.2 Redis

用途：

```text
Judge Queue
后续缓存
后续限流
后续临时状态
```

当前 Judge Stream：

```text
ojos:judge:submissions
```

当前 Consumer Group：

```text
judge-workers
```

Redis Stream 只负责任务投递，不是提交状态事实源。

---

### 8.3 Jaeger

用途：

```text
链路追踪
服务调用观察
请求耗时排查
```

访问：

```text
http://localhost:16686
```

当前已接入：

```text
Gateway
Auth
Problem API
Judge API
```

后续应继续补：

```text
Redis queue trace propagation
Judge Worker tracing
```

---

### 8.4 Docker Compose

用途：

```text
本地开发环境编排
服务启动
基础设施启动
容器网络
端口映射
volume 挂载
capability 配置
```

当前应包含：

```text
postgres
redis
jaeger
gateway
auth
problem-api
judge-api
judge-worker
```

当前不应包含：

```text
nats
```

---

## 九、NATS 移除后的架构变化

旧架构：

```text
judge-api
    ↓
NATS submission.created
    ↓
judge-worker
```

问题：

```text
NATS Core Pub/Sub 不持久化
worker 不在线时消息会丢
没有 ACK
没有 Pending List
不适合可靠判题任务
```

当前新架构：

```text
judge-api
    ↓
Redis Streams XADD
    ↓
judge-worker XREADGROUP
    ↓
XACK
```

优势：

```text
消息持久化
Consumer Group
Pending Entries List
ACK
积压可观测
多 worker 竞争消费
```

但即使使用 Redis Streams，OJOS 仍然保留：

```text
PostgreSQL PENDING 扫描
```

原因：

```text
Redis 是实时队列
PostgreSQL 是最终事实来源
```

最终可靠模型：

```text
Redis Streams
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
```

---

## 十、Judge 沙箱架构

Judge Worker 使用 `nsjail` 执行编译和运行。

当前沙箱目标：

```text
隔离文件系统
隔离进程空间
隔离网络
限制运行时间
限制地址空间
降低用户权限
阻止读取题目答案
阻止修改题目数据
```

当前 nsjail 运行边界：

```text
用户程序 uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
用户程序看不到题目 *.ans
用户程序不能覆盖题目 *.in / *.ans
每个 case 独立 /work
运行时 stdin/stdout/stderr 通过 /work 内文件重定向
```

Docker Compose 中 `judge-worker` 不使用：

```text
privileged: true
```

但需要最小 capability：

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
后续应接入 cgroup v2 统计峰值内存
```

---

## 十一、当前权限架构

Permission Core 是当前 OJOS 架构中的平台内核能力。

核心模型：

```text
principal
permission
scope
```

例如：

```text
user:2
judge.submit
system:0
```

当前权限判断顺序：

```text
1. super_admin 直接允许
2. 收集当前 scope / 父级 scope / type:0 / system:0
3. 检查直接 deny
4. 检查直接 allow
5. 检查全局 user_roles
6. 检查资源级 role_bindings
7. 默认拒绝
```

当前已经验证：

```text
普通 user 可以提交
deny judge.submit 后禁止提交
删除 deny 后恢复提交
```

后续所有模块都应接入 Permission Core。

---

## 十二、当前安全边界

当前安全边界如下：

```text
客户端不可信
Gateway 是认证入口
Gateway 注入的用户 Header 可信
下游服务信任 Gateway 后的 Header
PostgreSQL 是事实源
Redis Stream 是任务队列
storage/problems 是题目包存储
storage/submissions 是提交产物存储
judge-worker 通过 nsjail 执行不可信代码
```

必须注意：

```text
X-User-Id
X-Username
X-Roles
X-Auth-Verified
```

这些 Header 不能直接信任客户端传入。

Gateway 必须先清理，再重新注入。

业务服务不再信任请求体中的：

```text
user_id
```

而是从 Gateway 注入上下文读取当前用户。

---

## 十三、题型架构方向

OJOS 需要支持多种题型：

```text
传统题
交互题
通信题
提交答案题
启发式算法题
```

这些不应该全部写死在 judge-worker 中。

推荐抽象：

```text
problem_type
judge_plan
runner
checker
scorer
feedback_policy
```

---

### 13.1 传统题

传统题流程：

```text
编译
运行每个测试点
标准输入
标准输出
checker
汇总得分
```

当前已经打通：

```text
traditional-runner
default-trim-checker
default-sum-scorer
```

---

### 13.2 交互题

交互题需要：

```text
用户程序
交互器
双进程通信
实时 stdin/stdout 管道
超时控制
死锁检测
协议错误处理
```

不能用当前简单 runner 直接支持。

---

### 13.3 通信题

通信题需要：

```text
多个用户进程
多个输入输出通道
通信协议
统一资源限制
进程组管理
```

需要 Runner Core 支持多进程调度。

---

### 13.4 提交答案题

提交答案题不需要运行用户代码，而是：

```text
上传输出文件
checker 比较答案
按测试点或文件评分
```

应走不同 judge plan。

---

### 13.5 启发式算法题

启发式题需要：

```text
评分器
非 AC/WA 二值结果
相对得分
多次运行
随机种子
排行榜按分数排序
```

不应和传统题硬混在一起。

---

## 十四、赛制架构方向

OJOS 需要支持：

```text
ACM
OI
NOI
IOI
启发式比赛
更多自定义赛制
```

赛制不应该写死在 Judge Worker。

赛制核心包括：

```text
提交反馈策略
榜单规则
罚时规则
计分规则
封榜规则
滚榜规则
重测规则
权限规则
```

---

### 14.1 ACM

特点：

```text
AC / WA 二值反馈
通过题数优先
罚时排序
封榜
滚榜
气球
打印
ICPC Tools
```

需要模块：

```text
contest-rule-acm
scoreboard-acm
balloon-service
print-service
icpctools-adapter
```

---

### 14.2 OI / NOI

特点：

```text
部分分
子任务
捆绑点
总分排序
提交反馈可能可见
不一定有罚时
```

需要模块：

```text
contest-rule-oi
subtask-core
bundle-core
scorer-oi
```

---

### 14.3 IOI

特点：

```text
子任务
反馈策略复杂
可多次提交
分数可即时反馈
可能有 batch / output feedback
```

需要模块：

```text
contest-rule-ioi
feedback-policy-ioi
scorer-ioi
```

---

### 14.4 启发式比赛

特点：

```text
分数连续
可能越大越好或越小越好
可能需要归一化
可能有相对排名
可能需要多次运行
```

需要模块：

```text
contest-rule-heuristic
scorer-heuristic
leaderboard-heuristic
```

---

## 十五、Problem / Dataset / Judge 的关系

长期应拆分为：

```text
problem-core
dataset-core
judge-core
```

### 15.1 Problem Core

负责：

```text
题目基本信息
题面
标签
难度
可见性
权限
题目类型
题目归属
```

### 15.2 Dataset Core

负责：

```text
测试数据
测试点组
子任务
捆绑点
样例
文件存储
数据校验
数据权限
```

### 15.3 Judge Core

负责：

```text
提交
评测任务
评测结果
取消成绩
重测
状态流转
结果查询
```

当前阶段中：

```text
problem-api
```

已经开始承担 Problem / Dataset 的基础职责。

```text
judge-api + judge-worker
```

承担 Judge Core 的基础职责。

后续要继续把 runner / checker / scorer 抽象稳定下来。

---

## 十六、模块化总架构方向

长期架构建议分层：

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

---

### 16.1 Platform Kernel

包括：

```text
gateway
auth
permission-core
shared
module-registry
feature-flag-core
observability
```

职责：

```text
入口
认证
授权
基础设施
模块注册
功能启停
可观测性
```

---

### 16.2 Domain Core Modules

包括：

```text
problem-core
dataset-core
judge-core
runner-core
checker-core
scorer-core
contest-core
scoreboard-core
team-core
storage-core
```

职责：

```text
核心业务领域建模
稳定数据模型
提供基础业务接口
```

---

### 16.3 Capability Modules

包括：

```text
problem-type-traditional
problem-type-interactive
problem-type-communication
problem-type-output-only

contest-rule-acm
contest-rule-oi
contest-rule-noi
contest-rule-ioi
contest-rule-heuristic

checker-standard
checker-special
checker-float
checker-interactive

scorer-acm
scorer-oi
scorer-ioi
scorer-heuristic

balloon-service
print-service
forum-service
clarification-service
```

职责：

```text
具体题型能力
具体赛制能力
具体运营能力
```

---

### 16.4 Adapter Modules

包括：

```text
icpctools-adapter
polygon-adapter
import-export-adapter
remote-oj-adapter
vjudge-adapter
```

职责：

```text
兼容外部格式
导入导出
外部系统对接
```

---

### 16.5 Launcher / Module Registry

负责：

```text
模块发现
模块依赖检查
模块安装
模块卸载
模块启用
模块禁用
权限点注册
资源类型注册
Gateway 路由注册
数据库迁移注册
前端入口注册
配置生成
Docker Compose 片段生成
```

当前尚未实现。

当前不建议立刻做 Launcher，因为以下基础还没稳定：

```text
Problem Core
Dataset Core
Runner Core
Checker Core
Scorer Core
Contest Core
Gateway route registry
Permission API
Feature Flag
```

---

## 十七、当前开发优先级

当前不应该直接做 Contest Core。

原因是 Contest 依赖：

```text
题目模型
数据模型
提交模型
评测结果模型
权限模型
榜单模型
反馈策略
```

这些还没有完全稳定。

当前也不应该直接做 Module Registry。

原因是模块注册依赖：

```text
资源类型注册
权限点注册
Gateway 路由注册
数据库迁移注册
前端入口注册
Feature Flag
服务启动配置
```

这些都需要先等核心模块边界稳定。

当前推荐优先级：

```text
1. 完成文档重构
2. 多语言验收：c11 / python3 / java17
3. memory_kb 接入 cgroup v2 统计
4. scorer 抽象：ACM / IOI / Subtask / Bundle
5. checker 抽象：default / special judge
6. runner 抽象：traditional / interactive / communication / output-only
7. Problem Core / Dataset Core 深化
8. contest-core
9. contest-rule-acm
10. scoreboard-acm
11. permission-api
12. module-registry
13. feature-flag-core
14. launcher
```

---

## 十八、统一错误响应方向

当前系统里仍有部分错误响应需要统一。

后续建议统一为 JSON：

```json
{
  "code": 40301,
  "msg": "forbidden",
  "trace_id": "..."
}
```

建议错误码：

```text
40001 invalid request
40101 missing authorization header
40102 invalid token
40301 forbidden
40401 not found
50001 internal server error
50201 bad gateway
```

统一错误响应应在 shared 中重新设计，但不要恢复旧的 `shared/response` 粗暴封装，应按 go-zero 的错误处理方式重新整理。

---

## 十九、当前验收标准

当前 OJOS 架构可认为通过核心原型验收，需要满足：

```text
docker compose up -d --build 成功
NATS 容器不存在
Gateway /health 返回 ok
Auth login 返回 token
Problem API 可创建题目包
Judge submit 返回 PENDING
Judge Worker 消费 Redis Stream
Submission 最终 ACCEPTED
WA / CE / RE / TLE 正常
default-trim-checker 正常
用户程序不能读取 ans
cancel 正常
rejudge 正常
cases API 正常
Redis XPENDING 可观测
go build shared/auth/gateway/problem-api/judge-api 成功
cargo build judge-worker 成功
```

NATS 清理检查：

```powershell
Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

---

## 二十、当前架构结论

当前 OJOS 已经完成第一阶段核心闭环：

```text
认证
网关
权限
题目包
提交
队列
沙箱判题
结果
```

当前系统已经不是空架子，而是可以真实跑通：

```text
登录
鉴权
建题
提交
Redis Streams 排队
Rust Worker nsjail 判题
PostgreSQL 回写摘要
result.json 保存完整结果
查询结果
```

的 OJ 核心原型。

当前最重要的架构成果是：

```text
Permission Core v1
Problem Package
Redis Streams Judge Queue
Gateway 用户上下文透传
Judge API 提交 / cancel / rejudge
Judge Worker nsjail Pipeline
数据库原子抢任务
submission 文件化存储
result.json 结果格式
```

当前最重要的架构风险是：

```text
memory_kb 尚未统计
Runner / Checker / Scorer 抽象还需要继续稳定
Problem / Dataset 模型还需要继续深化
错误响应未统一
Contest / Scoreboard 尚未开始
Module Registry 尚未开始
```

因此下一阶段应该从“跑通”转向：

```text
模型稳定
边界稳定
错误稳定
执行安全稳定
数据管理稳定
Runner / Checker / Scorer 抽象稳定
```

具体落点是：

```text
文档重构
多语言验收
cgroup v2 memory 统计
scorer 抽象
checker 抽象
runner 抽象
Problem Core / Dataset Core 深化
```

完成这些后，再进入：

```text
Contest Core
Scoreboard Core
ACM / OI / IOI 赛制
模块注册
Launcher
```

---

## 二十一、最终方向

OJOS 最终应该演进为：

```text
一个可以通过模块组合搭建不同类型 OJ 的平台
```

例如：

```text
只安装 problem-core + judge-core + runner-core
    -> 普通题库 OJ

再安装 contest-core + contest-rule-acm + scoreboard-acm
    -> ACM 比赛系统

再安装 contest-rule-oi + subtask-core + scorer-oi
    -> OI / NOI 比赛系统

再安装 problem-type-interactive + runner-interactive
    -> 交互题平台

再安装 balloon-service + print-service + icpctools-adapter
    -> ICPC 现场赛系统

再安装 module-registry + launcher
    -> 可视化模块化 OJOS 平台
```

这才是 OJOS 与普通 OJ 的根本区别。
