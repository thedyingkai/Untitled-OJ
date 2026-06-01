# OJOS 架构总览文档

## 一、项目整体定位

OJOS，全称：

```text
Online Judge Operating System
```

它不是一个传统意义上的单体 Online Judge 网站，而是一个面向长期扩展的 OJ 基础设施平台。

传统 OJ 通常关注：

```text
用户注册
题目列表
提交代码
返回结果
比赛榜单
```

OJOS 的目标更大。它希望成为一个可以持续扩展、可以安装模块、可以支持多赛制、多题型、多运行器、多权限范围、多运营能力的 Online Judge 基础平台。

OJOS 的长期目标包括：

```text
支持 OI / NOI / ACM / IOI / 启发式算法题
支持传统题 / 交互题 / 通信题 / 提交答案题
支持子任务 / 捆绑点 / 部分分 / 多反馈策略
支持滚榜 / 封榜 / ICPC Tools 兼容格式
支持赛时发气球
支持打印代码 / 打印小票
支持题库
支持比赛组建
支持分级权限
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

OJOS 当前采用以下核心原则。

### 2.1 Everything is a Module

OJOS 中的能力应尽量模块化。

例如：

```text
auth
gateway
permission-core
judge-core
runner-core
problem-core
dataset-core
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
judge-api
judge-worker
problem-api
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
checker-standard
checker-special
scorer-oi
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
Can(user:3, "contest.manage", contest:5)
Can(user:4, "scoreboard.roll", contest:5)
Can(user:5, "module.install", system:0)
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
某人可以重测某个提交
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
        注入 X-User-Id

    judge-api:
        检查 judge.submit @ system:0
```

又如：

```text
POST /api/problem/problems/:id/testcases
    Gateway:
        检查 JWT
        注入 X-User-Id

    problem-api:
        检查 problem.manage.data @ problem:{id}
```

这样可以保证：

```text
新增模块不需要修改 Gateway 代码
Gateway 不耦合业务表
业务服务拥有最完整的业务上下文
Permission Core 统一负责授权判断
```

---

### 2.4 PostgreSQL is the Source of Truth

OJOS 当前使用 PostgreSQL 作为事实源。

所有关键业务状态最终必须落在 PostgreSQL 中。

包括：

```text
用户
角色
权限
题目
测试点
提交
测试点结果
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

### 2.5 Redis Streams for Reliable Tasks

当前 Judge 队列已经从 NATS Core Pub/Sub 迁移到 Redis Streams。

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

### 2.6 Do Not Over-Abstract Too Early

OJOS 的目标是模块化，但不能为了“看起来架构高级”提前抽象所有东西。

当前已经验证过的问题是：

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
 ┌───────────────┬────────────────┐
 │               │                │
Auth :8081   Judge API :8082   Future Services
 │               │
 │               ↓
 │          Redis Stream
 │       ojos:judge:submissions
 │               ↓
 │        Judge Worker
 │               ↓
 └────────── PostgreSQL ──────────┘

Observability:
    OpenTelemetry -> Jaeger

Cache / Queue:
    Redis

Removed:
    NATS
```

当前服务列表：

```text
frontend
gateway
auth
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

当前主要用于后续开发预留。

前端未来职责：

```text
登录 / 注册
题目列表
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
代理 /api/judge/*
JWT 验证
AuthMode
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

当前已删除：

```text
shared/config
shared/response
shared/events
shared/events/nats.go
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
```

---

### 4.6 Judge API

路径：

```text
services/judge-api
```

Judge API 是判题 HTTP API 层。

当前职责：

```text
创建题目 MVP
添加测试点 MVP
创建提交
查询提交
查询测试点结果
读取用户上下文
检查 judge.submit
写入 submissions
Redis XADD 投递任务
```

后续题目和测试点管理应迁移到：

```text
problem-api
dataset-core
```

---

### 4.7 Judge Worker

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
编译代码
运行测试点
比较输出
写回结果
XACK 消息
```

当前不安全，后续需要 Runner Core / sandbox。

---

## 五、当前数据流

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

### 5.3 用户提交代码

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
INSERT submissions(status=PENDING)
    ↓
Redis XADD ojos:judge:submissions
    ↓
返回 submission_id
```

---

### 5.4 Worker 判题

```text
judge-worker
    ↓
XREADGROUP ojos:judge:submissions
    ↓
解析 submission_id
    ↓
UPDATE submissions
        SET status='RUNNING'
        WHERE id=? AND status='PENDING'
        RETURNING id
    ↓
load submission / problem / test_cases
    ↓
compile
    ↓
run test cases
    ↓
compare output
    ↓
INSERT submission_cases
    ↓
UPDATE submissions final status
    ↓
XACK
```

---

### 5.5 查询提交结果

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
返回 status / score / time / memory / message
```

当前查询权限还需要后续完善：

```text
submission.view.own
submission.view.all
比赛反馈策略
封榜策略
```

---

## 六、当前数据库架构

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

### 6.1 Auth 表

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

### 6.2 Permission 表

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

### 6.3 Judge 表

```text
problems
test_cases
submissions
submission_cases
```

用于：

```text
MVP 题目
MVP 测试点
提交记录
评测结果
```

后续 `problems / test_cases` 应迁移或重构为：

```text
problem-core
dataset-core
```

---

## 七、当前基础设施

### 7.1 PostgreSQL

用途：

```text
事实源
权限数据
提交状态
评测结果
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

### 7.2 Redis

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

---

### 7.3 Jaeger

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

---

### 7.4 Docker Compose

用途：

```text
本地开发环境编排
服务启动
基础设施启动
容器网络
端口映射
```

当前应包含：

```text
postgres
redis
jaeger
gateway
auth
judge-api
judge-worker
```

当前不应包含：

```text
nats
```

---

## 八、NATS 移除后的架构变化

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

## 九、当前权限架构

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

## 十、当前安全边界

当前安全边界如下：

```text
客户端不可信
Gateway 是认证入口
Gateway 注入的用户 Header 可信
下游服务信任 Gateway 后的 Header
PostgreSQL 是事实源
Redis Stream 是任务队列
judge-worker 当前不安全
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

Judge API 不再信任请求体中的：

```text
user_id
```

而是从 Gateway 注入上下文读取当前用户。

---

## 十一、当前最大风险

当前最大风险是：

```text
Judge Worker 没有安全沙箱
```

用户代码直接运行在 judge-worker 容器中，可能：

```text
读容器文件
写大量文件
占满 CPU
占满内存
创建大量进程
访问网络
影响其他任务
攻击容器环境
```

所以当前系统不应直接开放给不可信公网用户。

后续必须优先推进：

```text
Runner Core
Sandbox Provider
network none
CPU limit
memory limit
pids limit
filesystem isolation
output limit
timeout kill
```

Runner 安全隔离不能一直放在最后。

---

## 十二、模块化总架构方向

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

### 12.1 Platform Kernel

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

### 12.2 Domain Core Modules

包括：

```text
problem-core
dataset-core
judge-core
runner-core
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

### 12.3 Capability Modules

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

### 12.4 Adapter Modules

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

### 12.5 Launcher / Module Registry

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
Contest Core
Gateway route registry
Permission API
Feature Flag
```

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

适合当前 MVP 模型。

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
scorer-oi
subtask-core
bundle-core
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

当前 MVP 中：

```text
judge-api
```

暂时包含：

```text
problems
test_cases
submissions
```

但长期应拆分为：

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
重测
状态流转
```

这样才能稳定支持未来多题型和多赛制。

---

## 十六、当前开发优先级

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

这些还没有稳定。

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
1. 清理 NATS 残留
2. 更新文档
3. 统一错误响应
4. Problem Core / Dataset Core 正规化
5. problem-api 接入 Permission Core
6. judge-api 移除 problem / test-case 管理职责
7. Runner Core 抽象
8. 基础安全隔离
9. 测试数据文件化
10. checker-standard
11. Special Judge
12. 子任务 / 捆绑点
13. contest-core
14. contest-rule-acm
15. scoreboard-acm
16. module-registry
17. feature-flag-core
18. launcher
```

---

## 十七、统一错误响应方向

当前系统里还有一些错误直接返回普通字符串，例如：

```text
forbidden
unauthorized
bad gateway
```

后续需要统一为 JSON：

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

统一错误响应应在：

```text
shared/errors
shared/httpresp
```

或类似模块中实现，但不要恢复旧的 `shared/response`，应重新按 go-zero 方式设计。

---

## 十八、当前验收标准

当前 OJOS 架构可认为通过 MVP 验收，需要满足：

```text
docker compose up -d --build 成功
NATS 容器不存在
Gateway /health 返回 ok
Auth login 返回 token
Auth profile 返回用户信息
Judge submit 返回 PENDING
Judge Worker 消费 Redis Stream
Submission 最终 ACCEPTED
Redis XPENDING 为 0
Permission deny judge.submit 生效
删除 deny 后恢复
go build shared/auth/gateway/judge-api 成功
cargo build judge-worker 成功
npm build frontend 成功
```

NATS 清理检查：

```powershell
Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

---

## 十九、当前架构结论

当前 OJOS 已经完成第一阶段核心闭环：

```text
认证
网关
权限
提交
队列
判题
结果
```

当前系统已经不是空架子，而是可以真实跑通：

```text
登录
鉴权
提交
Redis Streams 排队
Rust Worker 判题
PostgreSQL 回写
查询结果
```

的 OJ 原型。

当前最重要的架构成果是：

```text
Permission Core v1
Redis Streams Judge Queue
Gateway 用户上下文透传
Judge Worker PENDING 兜底恢复
数据库原子抢任务
```

当前最重要的架构风险是：

```text
Runner 安全隔离未完成
Problem / Dataset 模型未正规化
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
```

具体落点是：

```text
统一错误响应
Problem Core
Dataset Core
Runner Core
安全沙箱
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

## 二十、最终方向

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
