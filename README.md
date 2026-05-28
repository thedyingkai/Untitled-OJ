# Untitled-OJ
An OJ Based on Go

# OJOS 当前开发进度总结

# 一、项目定位

当前项目目标为构建：

```text
OJ Operating System（OJOS）
```

即：

```text
Online Judge Infrastructure Platform
```

而非传统单体 Online Judge。

核心理念：

```text
Everything is a Module
Everything is Event-Driven
Everything is Extensible
```

---

# 二、当前总体架构

当前已经开始形成：

```text
Frontend
    ↓
Gateway
    ↓
Infrastructure
    ↓
PostgreSQL / Redis / NATS
```

基础架构。

---

# 三、当前目录结构

目前 Monorepo 已初步建立：

```text
ojos/

├── frontend/
│
├── services/
│   ├── gateway/
│   ├── auth/
│   └── shared/
│
├── deploy/
│   ├── compose/
│   ├── migrations/
│   └── observability/
│
├── proto/
├── scripts/
└── docs/
```

---

# 四、当前已完成基础设施

## 1. Docker Compose 基础编排

技术：

* Docker Compose

已完成：

* PostgreSQL 容器
* Redis 容器
* NATS 容器
* Jaeger 容器
* Gateway 容器

实现：

```text
本地一键启动完整基础设施
```

命令：

```bash
docker compose up -d --build
```

---

## 2. PostgreSQL 数据库

技术：

* PostgreSQL 17
* pgxpool

已完成：

* Docker 化部署
* 健康检查
* 数据库初始化
* 数据库连接池
* 生命周期管理

当前数据库：

```text
ojos
```

---

## 3. Migration 系统

技术：

* golang-migrate

已完成：

* migration 初始化
* schema_migrations 管理
* users / roles / user_roles 表
* migration version 管理

当前 migration：

```text
000001_init_schema
```

---

## 4. RBAC 权限系统（基础）

已完成：

### users

```sql
users
```

### roles

```sql
roles
```

### user_roles

```sql
user_roles
```

当前默认角色：

| 角色          | 描述    |
| ----------- | ----- |
| super_admin | 超级管理员 |
| admin       | 管理员   |
| user        | 普通用户  |

---

# 五、Gateway 微服务

## 技术栈

| 模块        | 技术            |
| --------- | ------------- |
| Language  | Go            |
| HTTP      | net/http      |
| Config    | Viper         |
| Logger    | Zap           |
| DB Driver | pgx           |
| Trace     | OpenTelemetry |

---

## 已完成能力

### 1. HTTP Server

当前：

```text
Gateway 已可监听 8080
```

并成功运行：

```text
/health
```

接口。

---

### 2. Config 配置系统

技术：

* Viper

已完成：

* YAML 配置读取
* 环境变量支持
* Docker 容器配置加载

配置文件：

```text
services/gateway/configs/config.yaml
```

---

### 3. PostgreSQL Pool

技术：

* pgxpool

已完成：

* 最大连接数配置
* 生命周期管理
* Ping 检查
* 自动关闭

日志：

```json
postgres pool connected
```

---

### 4. Structured Logger

技术：

* Zap

已完成：

* JSON 日志
* service 字段
* trace_id 字段
* span_id 字段

当前日志示例：

```json
{
  "msg":"http request",
  "trace_id":"...",
  "span_id":"..."
}
```

---

# 六、Observability

这是当前最重要的基础设施之一。

---

## 1. Jaeger 分布式追踪

技术：

* OpenTelemetry
* Jaeger

已完成：

* Trace Exporter
* Jaeger Collector
* HTTP Span
* Gateway Span

当前：

```text
http://localhost:16686
```

可查看：

* HTTP 请求链路
* Trace Timeline
* Span Duration
* Service Dependency

---

## 2. HTTP Tracing

技术：

* otelhttp

已完成：

* HTTP 自动 Span
* 请求 trace_id
* 请求 span_id

当前请求：

```text
GET /health
```

已进入 Jaeger。

---

## 3. 日志与 Trace 关联

当前：

```text
trace_id
```

已经成功注入日志。

实现：

```text
日志 ←→ Jaeger Trace
```

关联。

这是后续：

* Submission
* Judge
* Contest
* MQ

调试的关键。

---

# 七、NATS Event Bus

## 技术

* NATS

---

## 当前状态

NATS 已完成：

* Docker 化
* 服务启动
* 网络联通

---

## 后续用途

未来用于：

* SubmissionCreated
* JudgeFinished
* ContestStarted

等事件。

---

# 八、Redis 缓存系统

## 技术

* Redis 8

---

## 当前状态

Redis 已：

* Docker 化
* 网络联通
* 可被服务访问

---

## 后续用途

未来用于：

* Rank Cache
* Session
* Contest Cache
* WebSocket

等能力。

---

# 九、当前已解决的重要工程问题

## 1. Docker Compose 网络

解决：

* 容器间通信
* localhost 误用
* container name

问题。

---

## 2. Go Monorepo

解决：

* replace
* 多 module
* shared module

问题。

---

## 3. Docker Build Context

解决：

* COPY 路径
* configs
* shared module

问题。

---

## 4. OpenTelemetry Schema 冲突

解决：

* semconv version mismatch

问题。

---

## 5. Viper Config Path

解决：

* configs.yaml
* config.yaml
* 容器路径

问题。

---

# 十、当前系统能力总结

Step 1 结束后：

OJOS 已经具备：

```text
真正可运行的基础微服务架构
```

而不是：

```text
普通单体 demo
```

---

# 十一、完成状态

| 模块             | 状态 |
| -------------- | -- |
| Docker Compose | ✅  |
| PostgreSQL     | ✅  |
| Redis          | ✅  |
| NATS           | ✅  |
| Jaeger         | ✅  |
| Gateway        | ✅  |
| Migration      | ✅  |
| Config System  | ✅  |
| Logger         | ✅  |
| Trace System   | ✅  |

---

# 十二、Step 2 当前进展

当前核心目标：

```text
基础服务层
```

即：

* Auth
* User
* Middleware
* RBAC
* Event
* Gateway Foundation

---

## 当前已完成

### 1. Shared Module

当前：

```text
services/shared
```

已建立。

用于：

* config
* logger
* middleware
* tracing

统一复用。

---

### 2. Gateway + Shared 解耦

当前 Gateway 已完成：

```text
基础设施与业务逻辑分离
```

实现：

* config shared
* tracing shared
* middleware shared

复用。

---

### 3. HTTP Middleware Logging

实现：

```text
HTTP Middleware Logging
```

当前日志：

```json
{
  "trace_id":"...",
  "span_id":"...",
  "method":"GET",
  "path":"/health"
}
```

说明：

```text
日志系统 + Trace 系统
```

已经真正联通。

---

### 4. Gateway Trace 全链路

当前：

```text
HTTP Request
    ↓
Middleware
    ↓
Trace Span
    ↓
Jaeger
```

已经形成完整链路。

---

# 十三、当前系统状态

当前 OJOS 已具备：

```text
Infrastructure Layer
```

能力。

已经不是：

```text
简单后端 demo
```

而是：

```text
真正具备可扩展性的微服务基础平台
```

---

# 十四、下一阶段

下一步将进入：

```text
基础业务服务
```

---

## 即将实现

### Auth Service

功能：

* register
* login
* bcrypt
* jwt

---

### User Service

功能：

* profile
* role
* permission

---

### Middleware

功能：

* JWT Middleware
* RBAC Middleware
* Auth Middleware

---

### Event Driven

功能：

* NATS Publish
* NATS Subscribe
* UserRegistered Event

---

### Gateway

功能：

* route group
* middleware chain
* error handler
* request validation

---

# 十五、当前技术栈总结

| 模块        | 技术             |
| --------- | -------------- |
| Backend   | Go             |
| Config    | Viper          |
| Logger    | Zap            |
| Trace     | OpenTelemetry  |
| Trace UI  | Jaeger         |
| DB        | PostgreSQL     |
| Cache     | Redis          |
| MQ        | NATS           |
| Migration | golang-migrate |
| Deploy    | Docker Compose |

---

# 十六、当前阶段评价

当前已经完成：

```text
Infrastructure Foundation
```

即：

```text
OJOS 基础设施底座
```

已经具备：

* 服务编排
* 配置系统
* 日志系统
* Trace 系统
* 数据库系统
* Migration 系统
* Gateway 系统

这是后续：

* Judge
* Contest
* Submission
* WebSocket
* Module System

的基础。
