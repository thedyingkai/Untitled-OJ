# Deployment 文档

## 一、文档定位

本文档记录 OJOS 当前阶段的本地部署方式。

当前 OJOS 使用：

```text
Docker Compose
```

作为本地开发和集成测试环境。

本文档覆盖：

```text
服务组成
端口规划
环境变量
volume 挂载
Docker Compose 启动
单服务重建
数据库迁移
Redis Streams 检查
Jaeger 检查
Judge Worker nsjail 配置
日志查看
常见部署问题
部署验收清单
```

本文档不覆盖：

```text
Kubernetes
公网生产部署
HTTPS 网关
对象存储
多机 Judge Worker
CI/CD
自动扩缩容
正式比赛部署
```

当前 OJOS 仍处于本地开发和核心原型阶段。

---

## 二、当前部署目标

当前部署目标是让本地环境可以完整跑通：

```text
Gateway
Auth
Problem API
Judge API
Judge Worker
PostgreSQL
Redis
Jaeger
storage/problems
storage/submissions
```

并完成：

```text
用户登录
JWT 鉴权
Gateway 反向代理
Problem API 管理题目包
Judge API 创建提交
Redis Streams 投递任务
Judge Worker 消费任务
nsjail 编译运行
result.json 落盘
submissions 摘要更新
Jaeger 观察链路
```

当前不再部署：

```text
NATS
```

Judge Queue 已经迁移为：

```text
Redis Streams
```

---

## 三、部署目录

Compose 目录固定为：

```text
D:\Untitled-OJ\deploy\compose
```

启动、关闭、查看 Compose 服务时，应先进入该目录：

```powershell
cd D:\Untitled-OJ\deploy\compose
```

不要在项目根目录直接执行：

```powershell
docker compose ps
docker compose logs
```

除非根目录也存在 `docker-compose.yml`。

否则会出现：

```text
no configuration file provided: not found
```

正确方式：

```powershell
cd D:\Untitled-OJ\deploy\compose
docker compose ps
```

---

## 四、当前服务列表

当前 Compose 应包含：

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

对应容器名建议：

```text
ojos-postgres
ojos-redis
ojos-jaeger
ojos-gateway
ojos-auth
ojos-problem-api
ojos-judge-api
ojos-judge-worker
```

不应包含：

```text
nats
ojos-nats
```

---

## 五、端口规划

当前本地端口建议如下：

| 服务            |    容器端口 |   宿主机端口 | 用途                     |
| ------------- | ------: | ------: | ---------------------- |
| `gateway`     |  `8080` |  `8080` | 对外统一入口                 |
| `auth`        |  `8081` |  `8081` | Auth 内部服务，调试可访问        |
| `judge-api`   |  `8082` |  `8082` | Judge API 内部服务，调试可访问   |
| `problem-api` |  `8083` |  `8083` | Problem API 内部服务，调试可访问 |
| `postgres`    |  `5432` |  `5433` | PostgreSQL             |
| `redis`       |  `6379` |  `6379` | Redis                  |
| `jaeger`      | `16686` | `16686` | Jaeger UI              |
| `jaeger`      |  `4317` |  `4317` | OTLP gRPC              |
| `jaeger`      |  `4318` |  `4318` | OTLP HTTP              |
| `jaeger`      | `14268` | `14268` | Jaeger collector       |

外部正常访问业务接口应走：

```text
http://localhost:8080
```

例如：

```text
http://localhost:8080/api/auth/login
http://localhost:8080/api/problem/...
http://localhost:8080/api/judge/...
```

---

## 六、服务内连接地址

容器内部不能使用宿主机的 `localhost` 访问其他容器。

容器内应使用 Compose service name。

### 6.1 PostgreSQL

宿主机迁移连接：

```text
postgres://postgres:password@localhost:5433/ojos?sslmode=disable
```

容器内服务连接：

```text
postgres://postgres:password@postgres:5432/ojos?sslmode=disable
```

### 6.2 Redis

宿主机调试连接：

```text
localhost:6379
```

容器内服务连接：

```text
redis://ojos-redis:6379/0
```

或按 service name：

```text
redis://redis:6379/0
```

当前实际日志中使用的是：

```text
redis://ojos-redis:6379/0
```

### 6.3 Jaeger

服务内 OTLP gRPC endpoint：

```text
ojos-jaeger:4317
```

或按 service name：

```text
jaeger:4317
```

当前服务配置中使用：

```yaml
Jaeger:
  Endpoint: ojos-jaeger:4317
```

---

## 七、Volume 挂载

当前必须挂载：

```text
../../storage:/data/ojos
```

用于让容器内服务访问：

```text
/data/ojos/problems
/data/ojos/submissions
```

宿主机路径：

```text
D:\Untitled-OJ\storage\problems
D:\Untitled-OJ\storage\submissions
```

容器内路径：

```text
/data/ojos/problems
/data/ojos/submissions
```

### 7.1 problems

```text
storage/problems
```

保存题目包。

例如：

```text
storage/problems/2-a-plus-b/problem.yaml
storage/problems/2-a-plus-b/tests/cases.yaml
storage/problems/2-a-plus-b/tests/001.in
storage/problems/2-a-plus-b/tests/001.ans
```

### 7.2 submissions

```text
storage/submissions
```

保存提交源码、编译产物、case 输出和 `result.json`。

例如：

```text
storage/submissions/20/source/main.cpp
storage/submissions/20/build/compile.log
storage/submissions/20/cases/001/stdout.txt
storage/submissions/20/result.json
```

### 7.3 Git 规则

不应提交运行时数据：

```text
storage/problems/*
storage/submissions/*
```

可以保留：

```text
storage/problems/.gitkeep
storage/submissions/.gitkeep
```

---

## 八、Docker Compose 推荐结构

`deploy/compose/docker-compose.yml` 应包含类似结构：

```yaml
services:
  postgres:
    image: postgres:17
    container_name: ojos-postgres
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: password
      POSTGRES_DB: ojos
    ports:
      - "5433:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres -d ojos"]
      interval: 5s
      timeout: 5s
      retries: 10

  redis:
    image: redis:8
    container_name: ojos-redis
    ports:
      - "6379:6379"

  jaeger:
    image: jaegertracing/all-in-one:latest
    container_name: ojos-jaeger
    environment:
      COLLECTOR_OTLP_ENABLED: "true"
    ports:
      - "16686:16686"
      - "4317:4317"
      - "4318:4318"
      - "14268:14268"

  gateway:
    build:
      context: ../../services
      dockerfile: gateway/Dockerfile
    container_name: ojos-gateway
    depends_on:
      postgres:
        condition: service_healthy
      jaeger:
        condition: service_started
    ports:
      - "8080:8080"

  auth:
    build:
      context: ../../services
      dockerfile: auth/Dockerfile
    container_name: ojos-auth
    depends_on:
      postgres:
        condition: service_healthy
      jaeger:
        condition: service_started
    ports:
      - "8081:8081"

  problem-api:
    build:
      context: ../../services
      dockerfile: problem-api/Dockerfile
    container_name: ojos-problem-api
    depends_on:
      postgres:
        condition: service_healthy
      jaeger:
        condition: service_started
    ports:
      - "8083:8083"
    volumes:
      - ../../storage:/data/ojos

  judge-api:
    build:
      context: ../../services
      dockerfile: judge-api/Dockerfile
    container_name: ojos-judge-api
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_started
      jaeger:
        condition: service_started
    ports:
      - "8082:8082"
    volumes:
      - ../../storage:/data/ojos

  judge-worker:
    build:
      context: ../../services
      dockerfile: judge-worker/Dockerfile
    container_name: ojos-judge-worker
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_started
    environment:
      DATABASE_URL: postgres://postgres:password@postgres:5432/ojos?sslmode=disable
      REDIS_URL: redis://ojos-redis:6379/0
      LANGUAGES_CONFIG: config/languages.yaml
    volumes:
      - ../../storage:/data/ojos
    cap_add:
      - SYS_ADMIN
      - SYS_CHROOT
      - SETUID
      - SETGID
      - NET_ADMIN
```

注意：

```text
Go 服务 build.context 应该是 ../../services
```

原因是：

```text
auth / gateway / problem-api / judge-api 都依赖 shared
Docker build 时必须能 COPY shared
```

不推荐写成：

```yaml
build:
  context: ../../services/gateway
```

否则 Dockerfile 无法访问：

```text
services/shared
```

---

## 九、Judge Worker nsjail 部署要求

`judge-worker` 需要在容器内使用 `nsjail` 运行用户程序。

Compose 中不要使用：

```yaml
privileged: true
```

当前使用最小 capability：

```yaml
cap_add:
  - SYS_ADMIN
  - SYS_CHROOT
  - SETUID
  - SETGID
  - NET_ADMIN
```

原因：

```text
nsjail 需要 mount namespace / chroot / uid gid / network namespace 相关能力
```

当前已验证的目标：

```text
jail 内 uid=10001 gid=10001
jail 内 /data/ojos/problems 不存在
/work 可写
用户程序无法读取题目答案文件
```

---

## 十、Judge Worker Dockerfile 要求

`services/judge-worker/Dockerfile` 需要提供：

```text
Rust 编译环境
nsjail
bash
coreutils
g++
gcc
python3
openjdk-17-jdk
/jail/root
/data/ojos/problems
/data/ojos/submissions
```

关键点：

```dockerfile
FROM rust:1.89-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        coreutils \
        g++ \
        gcc \
        python3 \
        openjdk-17-jdk \
    && rm -rf /var/lib/apt/lists/*
```

如果 Debian 源中没有 `nsjail` 包，不能直接：

```dockerfile
apt-get install nsjail
```

需要二选一：

```text
1. 在 Dockerfile 中从源码构建 nsjail
2. 使用已经包含 nsjail 的基础镜像
```

最终容器内必须满足：

```bash
nsjail --help
```

可以正常输出帮助信息。

---

## 十一、启动全部服务

进入 Compose 目录：

```powershell
cd D:\Untitled-OJ\deploy\compose
```

启动全部服务：

```powershell
docker compose up -d --build
```

查看状态：

```powershell
docker compose ps
```

预期：

```text
ojos-postgres       Up / healthy
ojos-redis          Up
ojos-jaeger         Up
ojos-gateway        Up
ojos-auth           Up
ojos-problem-api    Up
ojos-judge-api      Up
ojos-judge-worker   Up
```

---

## 十二、关闭服务

关闭当前 Compose 项目：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose down
```

关闭并清理 orphan：

```powershell
docker compose down --remove-orphans
```

清理 orphan 用于删除已经从 compose 文件中移除的服务，例如旧的：

```text
nats
```

---

## 十三、重建单个服务

### 13.1 重建并启动 gateway

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose build gateway
docker compose up -d gateway
```

### 13.2 重建并启动 auth

```powershell
docker compose build auth
docker compose up -d auth
```

### 13.3 重建并启动 problem-api

```powershell
docker compose build problem-api
docker compose up -d problem-api
```

### 13.4 重建并启动 judge-api

```powershell
docker compose build judge-api
docker compose up -d --no-deps judge-api
```

`--no-deps` 用于只重启当前服务，不重启 PostgreSQL / Redis / Jaeger。

### 13.5 重建并启动 judge-worker

```powershell
docker compose build judge-worker
docker compose up -d judge-worker
```

Judge Worker 依赖 Redis 和 PostgreSQL，通常可以不加 `--no-deps`。

---

## 十四、本地非 Docker 编译验证

### 14.1 shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 14.2 gateway

```powershell
cd D:\Untitled-OJ\services\gateway

go mod tidy
go build .
```

### 14.3 auth

```powershell
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 14.4 problem-api

```powershell
cd D:\Untitled-OJ\services\problem-api

go mod tidy
go build .
```

### 14.5 judge-api

```powershell
cd D:\Untitled-OJ\services\judge-api

go mod tidy
go build .
```

### 14.6 judge-worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo check
cargo build
```

---

## 十五、数据库迁移

迁移目录：

```text
deploy/migrations
```

宿主机执行迁移：

```powershell
cd D:\Untitled-OJ

migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

查看迁移版本：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  version
```

进入数据库查看：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

```sql
SELECT * FROM schema_migrations;
```

回滚一步：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  down 1
```

迁移规则：

```text
已经提交并执行过的 migration 不随意改历史
新增结构用新的 migration
down 文件谨慎写
初始化数据使用 ON CONFLICT DO NOTHING
权限点注册可重复执行
资源类型注册可重复执行
```

---

## 十六、日志查看

### 16.1 查看 gateway

```powershell
docker logs ojos-gateway --tail 100
```

### 16.2 查看 auth

```powershell
docker logs ojos-auth --tail 100
```

### 16.3 查看 problem-api

```powershell
docker logs ojos-problem-api --tail 100
```

### 16.4 查看 judge-api

```powershell
docker logs ojos-judge-api --tail 100
```

### 16.5 查看 judge-worker

```powershell
docker logs ojos-judge-worker --tail 100
```

正常启动应看到：

```text
judge-worker starting
connected redis successfully
redis stream consumer group already exists
judge-worker consuming redis stream
```

或者：

```text
redis stream consumer group created
judge-worker consuming redis stream
```

### 16.6 查看 postgres

```powershell
docker logs ojos-postgres --tail 100
```

### 16.7 查看 redis

```powershell
docker logs ojos-redis --tail 100
```

### 16.8 持续跟踪日志

```powershell
docker logs -f ojos-judge-worker
```

---

## 十七、Health 检查

### 17.1 Gateway

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/health"
```

预期：

```text
ok
```

或 JSON 中 `status = ok`。

### 17.2 Auth

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/health"
```

具体路径以当前 `auth.api` 和 Gateway route 为准。

### 17.3 Judge API

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/health"
```

具体路径以当前 `judgeapi.api` 为准。

### 17.4 Problem API

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/problem/health"
```

具体路径以当前 `problemapi.api` 为准。

---

## 十八、Redis Streams 检查

Judge 队列：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

查看 Stream 信息：

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

查看长度：

```powershell
docker exec -it ojos-redis redis-cli XLEN ojos:judge:submissions
```

正常状态：

```text
XPENDING = 0
```

说明 worker 已经消费并 XACK 消息。

注意：

```text
XACK 不会删除 Stream 历史消息
XRANGE 还能看到旧消息是正常的
```

---

## 十九、PostgreSQL 检查

进入数据库：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

查看表：

```sql
\dt
```

查看最近提交：

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
    message,
    code_path,
    result_path,
    judged_at,
    cancelled_at,
    cancel_reason
FROM submissions
ORDER BY id DESC
LIMIT 20;
```

查看题目包路径：

```sql
SELECT
    id,
    slug,
    title,
    package_dir,
    visibility,
    status
FROM problems
ORDER BY id;
```

查看旧表是否已删除：

```sql
SELECT to_regclass('public.test_cases') AS test_cases;
SELECT to_regclass('public.submission_cases') AS submission_cases;
```

预期：

```text
null
null
```

---

## 二十、Jaeger 检查

Jaeger UI：

```text
http://localhost:16686
```

服务配置中应使用：

```yaml
Jaeger:
  Endpoint: ojos-jaeger:4317
```

检查配置：

```powershell
Select-String `
  -Path D:\Untitled-OJ\services\*\etc\*.yaml `
  -Pattern "Jaeger|Endpoint|4317|4318|ojos-jaeger|jaeger"
```

查看 Jaeger 容器日志：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose logs jaeger --tail=80
```

正常情况下，服务请求后 Jaeger 中应能看到：

```text
gateway-service
auth-service
problem-api-service
judge-api-service
```

当前 Judge Worker 的完整 trace propagation 仍可后续完善。

---

## 二十一、NATS 清理检查

当前 OJOS 不再使用 NATS。

检查代码和配置：

```powershell
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期：

```text
无输出
```

检查 Compose：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose config | Select-String -Pattern "nats|4222"
```

预期：

```text
无输出
```

检查容器：

```powershell
docker ps --filter "name=nats"
```

预期：

```text
无输出
```

注意：

```text
event-listener 不是 NATS
zipkin 不是 NATS
```

不要误删无关依赖。

---

## 二十二、完整启动顺序

推荐首次启动顺序：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d postgres redis jaeger
```

等待 PostgreSQL healthy：

```powershell
docker compose ps
```

执行数据库迁移：

```powershell
cd D:\Untitled-OJ

migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

启动业务服务：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build gateway auth problem-api judge-api judge-worker
```

开发阶段也可以直接：

```powershell
docker compose up -d --build
```

---

## 二十三、完整重启顺序

修改多个服务后：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose down --remove-orphans
docker compose up -d --build
```

只改 Go API 服务：

```powershell
docker compose build judge-api
docker compose up -d --no-deps judge-api
```

只改 Rust Worker：

```powershell
docker compose build judge-worker
docker compose up -d judge-worker
```

只改配置文件：

```powershell
docker compose up -d --force-recreate judge-api
```

或对应服务名。

---

## 二十四、部署后 Judge 验收

### 24.1 登录

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

### 24.2 提交代码

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

### 24.3 查询结果

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

### 24.4 查看 Worker 日志

```powershell
docker logs ojos-judge-worker --tail 100
```

预期：

```text
received judge stream message
submission claimed
judge finished
judge stream message acked
```

---

## 二十五、常见问题

### 25.1 `no configuration file provided: not found`

原因：

```text
没有在 deploy/compose 目录执行 docker compose 命令
```

修法：

```powershell
cd D:\Untitled-OJ\deploy\compose
docker compose ps
```

---

### 25.2 Docker 拉镜像证书错误

可能报错：

```text
tls: failed to verify certificate: x509: certificate signed by unknown authority
```

常见原因：

```text
代理规则错误
公司 / 校园网代理劫持证书
Docker Desktop 未信任代理证书
Docker Hub 走了错误代理
```

优先检查：

```text
代理 rules
Docker Desktop proxy 设置
系统证书
Docker 是否能正常访问 registry-1.docker.io
```

这个问题通常不是 Dockerfile 写错。

---

### 25.3 build 成功但 up 后还是旧逻辑

原因可能是：

```text
没有重建对应服务
容器没有重新创建
启动的是旧 image
```

修法：

```powershell
docker compose build judge-worker
docker compose up -d --force-recreate judge-worker
```

或：

```powershell
docker compose down
docker compose up -d --build
```

---

### 25.4 judge-worker 一直不消费

排查：

```powershell
docker logs ojos-judge-worker --tail 100
docker exec -it ojos-redis redis-cli XINFO GROUPS ojos:judge:submissions
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
docker exec -it ojos-redis redis-cli XRANGE ojos:judge:submissions - +
```

再查数据库：

```sql
SELECT id, status, message
FROM submissions
ORDER BY id DESC
LIMIT 20;
```

常见原因：

```text
worker 没启动
Redis URL 错误
Consumer Group 没建
XREADGROUP timeout 被当成 fatal error
submission 没有进入 PENDING
try_claim_submission 抢不到
```

---

### 25.5 load cases.yaml failed

检查：

```powershell
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\problem.yaml" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\tests\cases.yaml" -Encoding UTF8
```

重点看：

```yaml
tests:
  root: tests
  cases: tests/cases.yaml
```

正确路径是：

```text
package_dir/tests/cases.yaml
```

不是：

```text
package_dir/tests/tests/cases.yaml
```

---

### 25.6 C++ 编译提示找不到 ld

错误：

```text
collect2: fatal error: cannot find 'ld'
```

修法：

```text
languages.yaml 中使用 /usr/bin/g++
C/C++ 编译参数加入 -B/usr/bin/
确认 /usr 被 bindmount_ro 到 jail
```

---

### 25.7 Runtime code 127

`127` 通常表示：

```text
command not found
```

检查：

```text
run.command 中 {exe} 是否替换
case_dir 中是否复制了 main
运行命令是否为 /work/main
```

---

### 25.8 stdout 为空

检查：

```text
stdin.txt 是否正确
运行命令是否在 jail 内重定向
stdout.txt 是否旧 root 文件导致 uid=10001 无法截断
case 运行前是否删除旧 stdout.txt / stderr.txt / checker.log
```

---

### 25.9 Jaeger 为空

检查：

```powershell
docker compose ps
docker logs ojos-jaeger --tail 80
Select-String -Path D:\Untitled-OJ\services\*\etc\*.yaml -Pattern "Jaeger|4317|ojos-jaeger"
```

确认：

```text
服务配置 endpoint = ojos-jaeger:4317
COLLECTOR_OTLP_ENABLED = true
服务确实收到请求
```

---

### 25.10 端口冲突

查看占用端口：

```powershell
netstat -ano | findstr :8080
netstat -ano | findstr :5433
netstat -ano | findstr :6379
```

解决方式：

```text
关闭占用程序
或修改 docker-compose.yml 的宿主机端口映射
```

---

## 二十六、部署验收清单

部署完成后，至少检查：

```text
docker compose ps 正常
ojos-postgres healthy
ojos-redis running
ojos-jaeger running
ojos-gateway running
ojos-auth running
ojos-problem-api running
ojos-judge-api running
ojos-judge-worker running
```

检查 NATS：

```text
docker ps --filter "name=nats" 无输出
docker compose config | Select-String "nats|4222" 无输出
```

检查数据库：

```text
schema_migrations 正常
test_cases 不存在
submission_cases 不存在
submissions 有 code_path / result_path
```

检查 Redis：

```text
XINFO GROUPS ojos:judge:submissions 正常
XPENDING ojos:judge:submissions judge-workers = 0
```

检查 Judge：

```text
提交 cpp17 A+B
最终 ACCEPTED
result.json 存在
stdout.txt = 3
checker.log = accepted
```

检查 Sandbox：

```text
用户程序不能读取 /data/ojos/problems
jail 内 uid/gid = 10001
/work 可写
```

检查 Jaeger：

```text
Gateway / Auth / Problem API / Judge API 至少能看到部分 trace
```

---

## 二十七、当前部署结论

当前 OJOS 本地部署模型是：

```text
Docker Compose
+
PostgreSQL
+
Redis Streams
+
Jaeger
+
Go API Services
+
Rust Judge Worker
+
nsjail
+
storage volume
```

当前关键边界是：

```text
Gateway 是统一入口
PostgreSQL 是事实源
Redis Streams 是 Judge 任务队列
storage/problems 是题目包
storage/submissions 是提交产物
Judge Worker 使用 nsjail 运行不可信代码
```

当前不再部署：

```text
NATS
```

当前部署适合：

```text
本地开发
接口调试
Judge 主链路验收
文档和架构验证
```

当前还不适合：

```text
公网开放
正式比赛
陌生用户提交
生产级长期运行
```

后续生产化前还需要补：

```text
统一配置管理
secret 管理
HTTPS
对象存储
日志持久化
指标监控
cgroup v2 memory 统计
输出大小限制
容器资源限制
备份恢复
CI/CD
多 worker 调度
权限管理后台
```
