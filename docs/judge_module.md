# OJOS Judge 模块开发文档

## 一、模块定位

Judge 模块是 OJOS 的核心评测模块，负责完成从“提交代码”到“返回评测结果”的完整链路。

当前 Judge 模块已经完成 MVP v0.1，具备最小可用评测能力：

```text
创建题目
添加测试点
提交代码
投递评测任务
Rust Worker 消费任务
根据 languages.yaml 选择语言
编译 / 运行 / 比较输出
回写总评测结果
回写测试点详情
查询提交结果
查询测试点结果
```

当前 Judge 模块不是最终安全版 Judge。它已经能真实判题，但用户代码仍直接运行在 `judge-worker` 容器内，后续必须补充隔离执行环境。

---

## 二、模块组成

当前 Judge 模块由两个服务组成：

```text
services/judge-api
services/judge-worker
```

### 2.1 judge-api

`judge-api` 使用 Go + go-zero 实现，负责：

```text
提供 HTTP API
写入题目数据
写入测试点数据
写入提交记录
查询提交结果
查询测试点结果
向 NATS 发布 submission.created 事件
```

当前监听端口：

```text
8082
```

### 2.2 judge-worker

`judge-worker` 使用 Rust 实现，负责：

```text
连接 NATS
订阅 submission.created
连接 PostgreSQL
读取提交代码
读取题目限制
读取测试点
根据 languages.yaml 获取语言配置
编译代码
运行测试点
比较输出
写入 submission_cases
更新 submissions
```

Judge Worker 是真正执行判题逻辑的核心模块。

---

## 三、当前目录结构

### 3.1 judge-api

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
│   │   ├── getsubmissionhandler.go
│   │   ├── getsubmissioncaseshandler.go
│   │   └── routes.go
│   │
│   ├── logic/
│   │   ├── addtestcaselogic.go
│   │   ├── createproblemlogic.go
│   │   ├── createsubmissionlogic.go
│   │   ├── getsubmissionlogic.go
│   │   └── getsubmissioncaseslogic.go
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
├── Dockerfile
├── go.mod
├── go.sum
├── judgeapi.api
└── judgeapi.go
```

### 3.2 judge-worker

```text
services/judge-worker/

├── config/
│   └── languages.yaml
│
├── src/
│   ├── config.rs
│   ├── db.rs
│   ├── event.rs
│   ├── judge.rs
│   └── main.rs
│
├── Dockerfile
├── Cargo.toml
└── Cargo.lock
```

---

## 四、数据库设计

Judge 模块当前使用四张表：

```text
problems
test_cases
submissions
submission_cases
```

### 4.1 problems

用于存储题目信息。

核心字段：

```text
id
title
time_limit_ms
memory_limit_mb
created_at
updated_at
```

当前字段说明：

| 字段                | 含义         |
| ----------------- | ---------- |
| `id`              | 题目 ID      |
| `title`           | 题目标题       |
| `time_limit_ms`   | 时间限制，单位毫秒  |
| `memory_limit_mb` | 内存限制，单位 MB |
| `created_at`      | 创建时间       |
| `updated_at`      | 更新时间       |

当前 `time_limit_ms` 已实际用于运行超时控制。

当前 `memory_limit_mb` 已存储，但尚未实际用于限制用户程序内存。

---

### 4.2 test_cases

用于存储测试点。

核心字段：

```text
id
problem_id
input
output
score
created_at
```

当前测试点输入输出直接存储在数据库中。

这适合 MVP 和小数据测试，但不适合大测试数据。后续需要改为文件化存储或对象存储。

---

### 4.3 submissions

用于存储提交记录和总评测结果。

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

| 字段           | 含义                        |
| ------------ | ------------------------- |
| `problem_id` | 题目 ID                     |
| `user_id`    | 用户 ID                     |
| `language`   | 提交语言，例如 `cpp17`、`python3` |
| `code`       | 用户代码                      |
| `status`     | 总评测状态                     |
| `score`      | 总得分                       |
| `time_ms`    | 最大测试点耗时                   |
| `memory_kb`  | 内存占用，当前暂未真实统计             |
| `message`    | 错误信息或补充信息                 |

---

### 4.4 submission_cases

用于存储每个测试点的评测结果。

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

该表用于支持：

```http
GET /judge/submissions/:id/cases
```

从而查询每个测试点的状态。

---

## 五、Migration

Judge 表结构由 migration 创建。

当前 migration 文件：

```text
deploy/migrations/000002_judge_schema.up.sql
deploy/migrations/000002_judge_schema.down.sql
```

创建命令：

```powershell
migrate create `
  -ext sql `
  -dir deploy/migrations `
  -seq judge_schema
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

## 六、Judge API

Judge API 由 go-zero 的 `.api` 文件生成。

API 描述文件：

```text
services/judge-api/judgeapi.api
```

代码生成命令：

```powershell
cd D:\Untitled-OJ\services\judge-api

goctl api go -api judgeapi.api -dir .
```

### 6.1 创建题目

```http
POST /judge/problems
Content-Type: application/json
```

请求体：

```json
{
  "title": "A+B Problem",
  "time_limit_ms": 1000,
  "memory_limit_mb": 256
}
```

响应：

```json
{
  "problem_id": 1
}
```

---

### 6.2 添加测试点

```http
POST /judge/test-cases
Content-Type: application/json
```

请求体：

```json
{
  "problem_id": 1,
  "input": "1 2\n",
  "output": "3\n",
  "score": 100
}
```

响应：

```json
{
  "test_case_id": 1
}
```

---

### 6.3 提交代码

```http
POST /judge/submissions
Content-Type: application/json
```

请求体：

```json
{
  "problem_id": 1,
  "user_id": 1,
  "language": "cpp17",
  "code": "#include <bits/stdc++.h>\nusing namespace std;\nint main(){int a,b;cin>>a>>b;cout<<a+b<<endl;}"
}
```

响应：

```json
{
  "submission_id": 1,
  "status": "PENDING"
}
```

提交成功后，`judge-api` 会：

```text
写入 submissions
设置 status = PENDING
发布 NATS 事件 submission.created
```

---

### 6.4 查询提交总结果

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

### 6.5 查询测试点详情

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

## 七、NATS 事件设计

当前 Judge 模块使用 NATS 投递评测任务。

### 7.1 Subject

```text
submission.created
```

### 7.2 事件结构

```json
{
  "id": "1780152953039547718",
  "type": "submission.created",
  "producer": "judge-api-service",
  "timestamp": "2026-05-30T14:55:53.039576815Z",
  "payload": {
    "submission_id": 2
  }
}
```

### 7.3 当前链路

```text
judge-api
    ↓
Publish submission.created
    ↓
NATS
    ↓
judge-worker
    ↓
handle_submission(submission_id)
```

### 7.4 当前限制

当前使用的是普通 NATS Core Pub/Sub。

如果 worker 离线期间 judge-api 发布消息，该消息会丢失。

后续需要补充：

```text
worker 启动后扫描 PENDING 任务
```

或者升级为：

```text
NATS JetStream
```

---

## 八、languages.yaml 多语言配置

Judge Worker 不在 Rust 代码中写死语言编译命令，而是从配置文件读取。

配置文件：

```text
services/judge-worker/config/languages.yaml
```

当前设计目标：

```text
一个 judge-worker 支持多种语言
新增语言优先修改 languages.yaml
Rust 判题核心不需要为每种语言写死分支
```

### 8.1 配置结构

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

字段说明：

| 字段                   | 含义                 |
| -------------------- | ------------------ |
| `source_file`        | 用户代码写入的源文件名        |
| `exe_file`           | 编译后可执行文件名，解释型语言可为空 |
| `compile.enabled`    | 是否需要编译             |
| `compile.command`    | 编译器命令              |
| `compile.args`       | 编译参数               |
| `compile.timeout_ms` | 编译超时               |
| `run.command`        | 运行命令               |
| `run.args`           | 运行参数               |

支持占位符：

| 占位符         | 含义       |
| ----------- | -------- |
| `{source}`  | 源文件路径    |
| `{exe}`     | 可执行文件路径  |
| `{workdir}` | 当前临时工作目录 |

---

### 8.2 当前支持语言

当前配置中已经包含：

```text
cpp17
cpp20
c11
python3
java17
rust
go
```

实际可用语言取决于 judge-worker Docker 镜像中是否安装了对应工具链。

当前 Dockerfile 安装：

```text
g++
gcc
python3
openjdk-17-jdk
golang-go
```

Rust 编译器由 `rust:1.89-bookworm` 基础镜像提供。

---

## 九、judge-worker 判题流程

Judge Worker 收到 `submission.created` 事件后执行：

```text
解析 submission_id
    ↓
更新 submissions.status = RUNNING
    ↓
读取 submissions
    ↓
读取 problems
    ↓
读取 test_cases
    ↓
根据 submission.language 查询 languages.yaml
    ↓
创建临时目录
    ↓
写入源代码文件
    ↓
如果需要编译，则执行 compile.command
    ↓
编译失败，返回 COMPILE_ERROR
    ↓
逐个测试点运行程序
    ↓
写入 stdin
    ↓
读取 stdout / stderr
    ↓
判断 TLE / RE / WA / AC
    ↓
写入 submission_cases
    ↓
更新 submissions 总状态
```

---

## 十、输出比较规则

当前比较规则是：

```text
统一换行符 CRLF -> LF
去除末尾空白
整体字符串完全相等
```

即：

```text
normalize_output(actual) == normalize_output(expected)
```

当前不支持：

```text
忽略所有空白
浮点误差
Special Judge
Interactor
```

---

## 十一、当前评测状态

当前使用的状态包括：

| 状态                     | 含义     |
| ---------------------- | ------ |
| `PENDING`              | 等待评测   |
| `RUNNING`              | 正在评测   |
| `ACCEPTED`             | 通过     |
| `WRONG_ANSWER`         | 答案错误   |
| `COMPILE_ERROR`        | 编译错误   |
| `RUNTIME_ERROR`        | 运行错误   |
| `TIME_LIMIT_EXCEEDED`  | 超时     |
| `SYSTEM_ERROR`         | 系统错误   |
| `UNSUPPORTED_LANGUAGE` | 不支持的语言 |

当前已经验证：

```text
ACCEPTED 正常
WRONG_ANSWER 正常
COMPILE_ERROR 正常
TIME_LIMIT_EXCEEDED 正常
```

---

## 十二、Docker Compose 集成

### 12.1 judge-api

```yaml
judge-api:
  build:
    context: ../../services
    dockerfile: judge-api/Dockerfile
  container_name: ojos-judge-api
  depends_on:
    postgres:
      condition: service_healthy
    nats:
      condition: service_started
  ports:
    - "8082:8082"
```

### 12.2 judge-worker

```yaml
judge-worker:
  build:
    context: ../../services
    dockerfile: judge-worker/Dockerfile
  container_name: ojos-judge-worker
  depends_on:
    postgres:
      condition: service_healthy
    nats:
      condition: service_started
  environment:
    NATS_URL: nats://ojos-nats:4222
    DATABASE_URL: postgres://postgres:password@postgres:5432/ojos?sslmode=disable
    LANGUAGES_CONFIG: config/languages.yaml
```

---

## 十三、Dockerfile

### 13.1 judge-api Dockerfile

```dockerfile
FROM golang:1.26.3

WORKDIR /app

COPY judge-api/go.mod judge-api/go.sum ./judge-api/

WORKDIR /app/judge-api
RUN go mod download

WORKDIR /app

COPY judge-api ./judge-api

WORKDIR /app/judge-api

RUN go build -o judge-api .

CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]
```

注意：二进制名必须和 CMD 一致。

正确：

```dockerfile
RUN go build -o judge-api .
CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]
```

错误：

```dockerfile
RUN go build -o judge.rs-api .
CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]
```

---

### 13.2 judge-worker Dockerfile

```dockerfile
FROM rust:1.89-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        g++ \
        gcc \
        python3 \
        openjdk-17-jdk \
        golang-go \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY judge-worker ./judge-worker

WORKDIR /app/judge-worker

RUN cargo build --release

CMD ["./target/release/judge-worker"]
```

说明：

```text
使用 rust:1.89-bookworm 是为了固定 Debian 版本。
不要使用会漂移到 trixie 的 rust:1.89，否则 openjdk-17-jdk 可能无法安装。
```

---

## 十四、当前验收命令

### 14.1 查看容器

```powershell
docker ps --filter "name=judge"
```

预期：

```text
ojos-judge-api
ojos-judge-worker
```

### 14.2 查看 judge-api 日志

```powershell
docker logs ojos-judge-api
```

预期：

```text
Starting server at 0.0.0.0:8082...
```

### 14.3 查看 judge-worker 日志

```powershell
docker logs ojos-judge-worker
```

预期：

```text
judge-worker starting
connected successfully
judge-worker subscribed submission.created
```

---

## 十五、当前验收结果

当前已经完成并验证：

```text
创建题目成功
添加测试点成功
提交代码成功
worker 收到 submission.created
worker 读取 submission
worker 根据 languages.yaml 判题
C++17 判题正常
Python3 判题正常
WA 正常
CE 正常
TLE 正常
submission_cases 写入正常
/judge/submissions/:id/cases 查询正常
```

当前可以确认：

```text
OJOS Judge MVP v0.1 API 闭环完成
```

---

## 十六、当前不是完整 Judge 的部分

当前 Judge MVP 仍然有重要限制。

### 16.1 没有安全沙箱

当前用户代码直接运行在 `judge-worker` 容器中。

风险：

```text
用户代码可能读取 worker 容器文件
用户代码可能占满 CPU
用户代码可能 fork 炸进程
用户代码可能写临时目录
用户代码可能影响后续评测
```

后续必须改为隔离执行：

```text
judge-worker
    ↓
启动受限 runner 容器
    ↓
限制 network / cpu / memory / pids / filesystem
```

---

### 16.2 任务消息不持久

当前使用 NATS Core Pub/Sub。

如果 worker 离线时产生提交，消息会丢失。

后续方案：

```text
方案一：worker 定期扫描 PENDING submissions
方案二：NATS JetStream 持久化任务
```

---

### 16.3 多 worker 并发不安全

当前如果部署多个 worker，可能重复评测同一条 submission。

后续需要抢任务锁：

```sql
UPDATE submissions
SET status = 'RUNNING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

只有成功更新的 worker 才能继续评测。

---

### 16.4 内存限制未实现

当前只存储了：

```text
memory_limit_mb
memory_kb
```

但尚未真实限制和统计内存。

后续需要通过 runner 容器或系统级资源限制实现。

---

### 16.5 测试数据仍存数据库

当前测试点输入输出存储在数据库 `TEXT` 字段中。

这只适合小数据。

后续应改为：

```text
数据库存测试点 metadata
输入输出文件存本地 volume / object storage
```

---

### 16.6 不支持 Special Judge

当前只支持标准输出比较。

未支持：

```text
SPJ
浮点误差
忽略空白
交互题
文件 IO
子任务计分
```

---

## 十七、下一阶段计划

建议后续开发顺序：

```text
1. worker 原子抢任务，避免重复评测
2. worker 启动扫描 PENDING，避免 NATS 消息丢失
3. 隔离 runner 容器
4. CPU / memory / pids / network 限制
5. 测试数据文件化
6. 支持 OI 计分模式
7. 支持 Special Judge
8. Gateway 接入 judge-api
9. 权限控制：普通用户提交，管理员建题
```

优先级最高的是：

```text
任务可靠性
隔离执行安全性
```

因为当前 Judge 已经能判题，下一步最重要的是让它稳定、安全。
