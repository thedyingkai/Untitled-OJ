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
支持多题型
支持多赛制
支持插件式模块安装
支持完整资源级权限控制
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
完整资源级权限核心
真实判题闭环
多语言评测配置
日志与链路追踪
Gateway 统一鉴权
用户上下文透传
Judge API 权限检查
Judge PENDING 兜底恢复
Judge 原子抢任务
```

当前可以认为已经完成：

```text
Infrastructure Foundation
Shared v0.3
Gateway v0.3
Auth v0.2
Permission Core v1
Judge API MVP v0.3
Judge Worker Reliability v0.2
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
| DB Driver              | pgx / pgxpool / sqlx             |
| Migration              | golang-migrate                   |
| Message Queue          | NATS                             |
| Cache                  | Redis                            |
| Tracing                | OpenTelemetry                    |
| Trace UI               | Jaeger                           |
| Logger                 | Zap / tracing                    |
| Deployment             | Docker Compose                   |
| Auth                   | JWT / bcrypt                     |
| Permission             | Resource-level RBAC / ACL        |
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

## 五、当前核心服务

```text
shared        公共基础库
gateway       统一 HTTP 网关
auth          认证服务
judge-api     判题 API 服务
judge-worker  Rust 判题执行器
```

说明：

```text
Permission Core 当前位于 shared/security/permission。
它不是独立 HTTP 服务，而是所有业务服务共享的权限核心库。
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
完整资源级权限
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
000003_permission_core
```

执行迁移：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

查看迁移状态：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

```sql
SELECT * FROM schema_migrations;
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

当前已经补充数据库兜底机制：

```text
worker 启动扫描 PENDING
worker 定时扫描 PENDING
worker 原子抢任务
```

因此，即使 NATS Core 消息丢失，历史 `PENDING` 提交也可以被 worker 后续扫描恢复。

当前仍需注意：

```text
NATS Core 本身不持久化消息
可靠任务最终仍建议升级到 JetStream / DB Queue / Redis Stream
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

当前 Shared 已完成 go-zero 适配、旧兼容层清理，并升级到 v0.3。

当前目录：

```text
services/shared/

├── database/
├── events/
├── logger/
├── middleware/
├── security/
│   ├── authctx/
│   ├── jwt/
│   └── permission/
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
JWT 生成与解析
可信用户上下文 Header 解析
完整资源级权限检查
角色绑定
直接授权 / 拒绝
资源继承关系维护
权限点注册
资源类型注册
```

Shared 当前原则：

```text
服务自己定义配置
shared 只接收参数并创建基础设施对象
业务逻辑不进入 shared
业务配置不进入 shared
新增业务模块不修改 shared 核心结构
```

---

## 八、Gateway 模块

路径：

```text
services/gateway
```

Gateway 是 OJOS 的统一 HTTP 入口。

当前 Gateway 已完成 go-zero 重构，并升级到 v0.3。

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
ReverseProxy Rewrite
Auth 服务代理
Judge API 服务代理
统一 JWT 鉴权
用户上下文透传
按路由配置 AuthMode
```

当前监听端口：

```text
8080
```

---

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

---

### 8.2 配置驱动代理

Gateway 当前通过 `gateway.yaml` 配置代理规则。

示例：

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

转发规则：

```text
/api/auth/login
    -> http://auth:8081/auth/login

/api/judge/submissions
    -> http://judge-api:8082/judge/submissions
```

---

### 8.3 AuthMode

当前支持三种鉴权模式：

| AuthMode   | 含义                |
| ---------- | ----------------- |
| `none`     | 不解析 token         |
| `optional` | 有 token 就解析，没有也放行 |
| `required` | 必须有合法 token       |

当前配置：

```text
/api/auth   optional
/api/judge  required
```

---

### 8.4 用户上下文透传

Gateway 验证 JWT 后，会清理客户端伪造的可信 Header，并重新注入：

```text
X-Auth-Verified: true
X-User-Id: 1
X-Username: admin
X-Roles: user
```

下游服务不应信任客户端直接传入的这些 Header，只应信任 Gateway 注入后的 Header。

---

### 8.5 Gateway 与权限边界

Gateway 不做具体业务权限判断。

Gateway 只负责：

```text
JWT 验证
用户上下文透传
```

业务服务负责选择具体权限点并调用 Permission Core。

例如：

```text
POST /judge/submissions
    -> judge.submit @ system:0

POST /problems
    -> problem.create @ system:0

POST /problems/:id/testcases
    -> problem.manage.data @ problem:{id}

POST /contests/:id/freeze
    -> contest.freeze @ contest:{id}
```

---

### 8.6 当前限制

Gateway 当前仍未实现：

```text
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

---

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

---

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

---

### 9.3 JWT

当前 JWT 能力已迁移到：

```text
services/shared/security/jwt
```

Auth 使用 shared JWT 签发 token，Gateway 使用 shared JWT 解析 token。

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

---

## 十、Permission Core 模块

路径：

```text
services/shared/security/permission
```

Permission Core 是 OJOS 的完整资源级权限核心，用于判断：

```text
谁可以在什么资源范围内执行什么操作
```

统一抽象为：

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

当前版本：

```text
Permission Core v1
```

---

### 10.1 当前能力

当前 Permission Core 已完成：

```text
完整资源级权限数据库模型
Principal / Scope 抽象
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs

保留并兼容 users / roles / user_roles

shared permission checker
HasUserPermission
RequireUserPermission
BindRole
AssignPermission
AddResourceEdge
RegisterResourceType
RegisterPermission
GrantRolePermission

judge-api 接入 judge.submit
普通 user 角色允许提交
permission_assignments.deny 可以覆盖普通角色权限
删除 deny 后权限恢复
```

---

### 10.2 Principal

权限主体使用：

```text
principal_type
principal_id
```

当前主要使用：

```text
user:{id}
```

未来可扩展：

```text
team:{id}
group:{id}
service:{id}
```

---

### 10.3 Scope

权限作用域使用：

```text
scope_type
scope_id
```

示例：

```text
system:0
problem:7
contest:3
group:2
team:5
submission:100
module:0
```

约定：

```text
system:0 表示全局作用域
problem:0 表示所有题目
contest:0 表示所有比赛
scope_id = 0 表示某类资源的全局范围
```

---

### 10.4 权限核心表

Permission Core 新增核心表：

```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

含义：

```text
resource_types          资源类型注册表
permissions             权限点注册表
role_permissions         角色拥有哪些权限
role_bindings            某个主体在某个资源范围内拥有某个角色
permission_assignments   直接授权 / 直接拒绝
resource_edges           资源继承关系
permission_audit_logs    权限变更审计日志
```

---

### 10.5 权限判断规则

权限判断顺序：

```text
1. 如果用户拥有 super_admin，则直接允许
2. 收集当前 scope、父级 scope、type:0、system:0
3. 检查 permission_assignments.deny
4. 检查 permission_assignments.allow
5. 检查全局 user_roles
6. 检查资源级 role_bindings
7. 默认拒绝
```

说明：

```text
deny 优先于普通 allow 和角色权限
super_admin 高于 deny
role_permissions 不带 scope
role_bindings 带 scope
```

---

### 10.6 当前内置资源类型

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

未来新增资源类型只需要注册到 `resource_types`，不需要修改权限核心表。

---

### 10.7 当前内置权限点

当前已内置：

```text
system.admin

module.install
module.enable
module.disable
module.configure

launcher.view
launcher.install
launcher.uninstall
launcher.enable
launcher.disable

problem.create
problem.view
problem.view.private
problem.edit
problem.delete
problem.manage.data
problem.manage.asset

judge.submit

submission.view.own
submission.view.all
submission.rejudge
submission.delete

contest.create
contest.view
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
contest.roll
contest.publish

scoreboard.view
scoreboard.view.admin
scoreboard.freeze
scoreboard.roll
scoreboard.export

balloon.manage
balloon.deliver

print.request
print.manage
print.operate

forum.post
forum.moderate

clarification.ask
clarification.answer
clarification.publish
```

未来新增模块只需要注册新的权限点，不需要修改权限核心表。

---

### 10.8 当前真实验证

当前已经验证：

```text
permtest 用户只有 user 角色
permtest 可以提交代码
submission 正确写入 user_id = 2
提交最终 ACCEPTED
写入 judge.submit @ system:0 deny 后提交被 forbidden 拦截
删除 deny 后提交恢复
```

该验证说明：

```text
普通 user 角色通过 role_permissions 获得 judge.submit
permission_assignments.deny 可以覆盖普通 user 角色权限
删除 deny 后权限恢复
judge-api 已经真实接入 shared permission checker
```

---

### 10.9 当前限制

当前 Permission Core 仍缺少：

```text
统一 JSON 错误响应
permission-api / admin API
权限管理前端
resource_edges 在 problem / contest 创建时的自动写入
权限审计日志查询接口
role revoke / permission revoke API
```

这些属于权限管理能力，不影响当前权限核心模型。

---

## 十一、Judge API 模块

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

说明：

```text
POST /judge/problems 和 POST /judge/test-cases 是早期 MVP 接口。
后续会迁移到 problem-api。
judge-api 最终只负责 submissions / submission_cases。
```

---

### 11.1 用户身份来源

当前 `POST /judge/submissions` 不再信任请求体里的 `user_id`。

旧请求：

```json
{
  "problem_id": 1,
  "user_id": 1,
  "language": "cpp17",
  "code": "..."
}
```

新请求：

```json
{
  "problem_id": 1,
  "language": "cpp17",
  "code": "..."
}
```

`judge-api` 通过 `UserContextMiddleware` 从 Gateway 注入的 Header 中读取用户身份：

```text
X-Auth-Verified
X-User-Id
X-Username
X-Roles
```

最终写入：

```text
submissions.user_id
```

因此前端无法通过伪造请求体 `user_id` 来替别人提交。

---

### 11.2 创建提交

```http
POST /judge/submissions
```

请求示例：

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
  "submission_id": 10,
  "status": "PENDING"
}
```

提交后，Judge API 会：

```text
检查 judge.submit @ system:0 权限
写入 submissions
发布 NATS 事件 submission.created
```

当前普通 `user` 角色默认拥有：

```text
judge.submit
```

因此普通登录用户可以提交代码。

如果在 `permission_assignments` 中对某个用户写入：

```text
deny judge.submit @ system:0
```

则该用户提交会被拒绝。

---

### 11.3 查询提交结果

```http
GET /judge/submissions/:id
```

响应示例：

```json
{
  "id": 10,
  "problem_id": 1,
  "user_id": 1,
  "language": "cpp17",
  "status": "ACCEPTED",
  "score": 100,
  "time_ms": 0,
  "memory_kb": 0,
  "message": ""
}
```

说明：

```text
0ms 在算法竞赛系统中是正常显示，不需要强制改为 1ms。
```

---

## 十二、Judge Worker 模块

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
启动扫描 PENDING
定时扫描 PENDING
原子抢任务
```

---

### 12.1 当前可靠性机制

当前 Judge Worker 已经从：

```text
只依赖 NATS 实时事件
```

升级为：

```text
NATS 实时事件
+
数据库 PENDING 兜底扫描
+
原子抢任务
```

核心机制：

```sql
UPDATE submissions
SET status = 'RUNNING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

只有成功返回一行的 worker 才能继续判题。

如果返回空，说明：

```text
任务已经被其他 worker 抢走
或任务已经被判完
或任务状态不再是 PENDING
```

当前该 worker 会跳过该 submission。

---

### 12.2 启动扫描

worker 启动后会扫描：

```sql
SELECT id
FROM submissions
WHERE status = 'PENDING'
ORDER BY id ASC
LIMIT $1;
```

这可以恢复由于以下原因导致的历史 PENDING：

```text
NATS 消息丢失
worker 重启
judge-api 发布事件失败
worker 不在线
旧版本 bug
```

---

### 12.3 定时扫描

worker 运行期间会周期性扫描 PENDING。

当前周期：

```text
10 秒
```

这使系统即使错过实时事件，也能在后续扫描中恢复任务。

---

### 12.4 当前评测状态

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
历史 PENDING 可恢复
新提交可正常评测
```

---

### 12.5 当前 Judge 限制

当前 Judge 是 MVP，不是安全生产级 Judge。

当前限制：

```text
用户代码直接运行在 judge-worker 容器内
没有独立安全沙箱
没有真实内存限制
测试点直接存数据库 TEXT 字段
不支持 Special Judge
不支持 OI 子任务计分
不支持交互题
不支持通信题
不支持提交答案题
```

后续必须补：

```text
runner 隔离容器
network none
cpu / memory / pids 限制
测试数据文件化
SPJ / checker
子任务 / 捆绑点
多题型执行计划
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

### 14.3 查看日志

```powershell
docker logs ojos-gateway
docker logs ojos-auth
docker logs ojos-judge-api
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

$token = $res.data.token
```

---

### 15.3 Auth Profile

```powershell
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

### 15.4 Judge 不带 token 应失败

```powershell
Invoke-WebRequest `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/3/cases"
```

预期：

```text
401
{"code":40101,"msg":"missing authorization header"}
```

---

### 15.5 Judge 提交代码

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
user_id = 1
score = 100
```

---

### 15.7 Permission deny 验证

创建普通用户 `permtest` 后，确认其只有 `user` 角色：

```sql
SELECT u.id, u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
WHERE u.username = 'permtest'
ORDER BY r.name;
```

写入 deny：

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

此时 `permtest` 再提交代码应被拒绝。

删除 deny：

```sql
DELETE FROM permission_assignments
WHERE principal_type = 'user'
  AND principal_id = (SELECT id FROM users WHERE username = 'permtest')
  AND permission_code = 'judge.submit'
  AND scope_type = 'system'
  AND scope_id = 0;
```

删除后 `permtest` 应恢复提交权限。

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

| 模块                         | 状态                  |
| -------------------------- | ------------------- |
| Docker Compose             | 完成                  |
| PostgreSQL                 | 完成                  |
| Redis                      | 完成                  |
| NATS                       | 完成                  |
| Jaeger                     | 完成                  |
| Migration                  | 完成                  |
| Shared                     | v0.3 完成             |
| Gateway                    | v0.3 完成             |
| Auth                       | v0.2 完成             |
| Permission Core            | v1 完成               |
| Judge API                  | MVP v0.3 完成         |
| Judge Worker               | Reliability v0.2 完成 |
| 多语言评测配置                    | MVP 完成              |
| Gateway 用户上下文透传            | 完成                  |
| Judge API 权限检查             | `judge.submit` 已接入  |
| Judge PENDING 恢复           | 完成                  |
| Judge 原子抢任务                | 完成                  |
| 安全沙箱                       | 未完成                 |
| 多题型系统                      | 未完成                 |
| 子任务 / 捆绑点                  | 未完成                 |
| Permission 管理 API / UI     | 未完成                 |
| Module Registry / Launcher | 未完成                 |

---

## 十八、当前不是完整生产级系统的部分

当前系统仍然存在以下关键缺口：

```text
Judge 没有安全沙箱
Judge 没有真实内存限制
测试数据没有文件化
没有 Special Judge
没有 OI 子任务计分
没有交互题
没有通信题
没有提交答案题
权限核心已完成，但缺少权限管理 API / UI
没有统一错误码体系
没有模块注册与启动器
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
1. 统一错误响应，尤其是 forbidden -> JSON
2. Problem Core / Dataset Core 正规化
3. problem-api 接入 Permission Core
4. 创建 problem 后自动绑定 problem_owner
5. 测试数据文件化
6. checker / special judge 抽象
7. 子任务 / 捆绑点
8. problem-type-traditional
9. contest-core
10. contest-rule-acm
11. scoreboard-acm
12. module-registry
13. feature-flag-core
14. runner 安全隔离
```

短期最建议进入：

```text
1. 统一错误响应
2. Problem Core / Dataset Core 正规化
3. problem-api 接入 Permission Core
```

---

## 二十、项目当前结论

OJOS 当前已经完成基础平台和核心 MVP：

```text
Gateway 可以作为统一入口
Auth 可以完成登录鉴权
Permission Core 已完成完整资源级权限核心
Judge 可以真实评测代码
Shared 已成为纯公共基础库
Docker Compose 可以启动完整本地环境
历史 PENDING 可以被恢复
新提交可以完成可信用户身份绑定和评测
judge-api 已接入 judge.submit 权限检查
```

当前系统已经具备继续开发：

```text
Problem
Dataset
Contest
Scoreboard
Training
Ranking
Submission
WebSocket
Frontend
Module Registry
Launcher
```

等模块的基础。

下一阶段重点应从“能跑通”转向：

```text
统一错误响应
数据模型正规化
题型扩展
赛制扩展
安全隔离
权限管理 API / UI
模块化安装
```
