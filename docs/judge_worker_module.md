# OJOS Judge Worker 模块开发文档

## 一、模块定位

`services/judge-worker` 是 OJOS 的判题执行模块。

它使用 Rust 编写，负责消费判题任务、读取提交代码、编译运行用户程序、执行测试点、计算评测结果，并将结果写回 PostgreSQL。

当前 Judge Worker 不是 HTTP 服务，而是一个后台任务进程。

---

## 二、当前版本状态

当前 Judge Worker 版本状态：

```text
Judge Worker Reliability v0.2
```

当前已完成：

```text
NATS 事件消费
PostgreSQL 连接
多语言配置加载
C++ / Python / Java / Go / Rust 等语言配置
编译阶段
运行阶段
标准输出比较
测试点结果写入
提交总结果写入
启动扫描 PENDING
定时扫描 PENDING
原子抢任务
历史 PENDING 恢复
```

当前已经验证：

```text
AC 正常
WA 正常
CE 正常
TLE 正常
历史 PENDING 自动恢复
新提交 PENDING -> ACCEPTED
多 worker 下具备基础防重复判题能力
```

---

## 三、模块目录结构

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
├── Cargo.toml
├── Cargo.lock
└── Dockerfile
```

---

## 四、运行环境

Judge Worker 通过 Docker Compose 启动。

依赖：

```text
PostgreSQL
NATS
languages.yaml
系统语言工具链
```

当前 Docker 镜像中安装：

```text
g++
gcc
python3
openjdk
golang-go
rust
```

实际支持语言取决于：

```text
config/languages.yaml
```

---

## 五、环境变量

Judge Worker 使用以下环境变量：

| 环境变量               | 说明            | 默认值                                                               |
| ------------------ | ------------- | ----------------------------------------------------------------- |
| `NATS_URL`         | NATS 地址       | `nats://ojos-nats:4222`                                           |
| `DATABASE_URL`     | PostgreSQL 地址 | `postgres://postgres:password@postgres:5432/ojos?sslmode=disable` |
| `LANGUAGES_CONFIG` | 语言配置文件路径      | `config/languages.yaml`                                           |

---

## 六、languages.yaml

路径：

```text
services/judge-worker/config/languages.yaml
```

该文件用于配置不同语言的源文件名、编译命令、运行命令。

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

| 占位符         | 含义      |
| ----------- | ------- |
| `{source}`  | 源文件路径   |
| `{exe}`     | 可执行文件路径 |
| `{workdir}` | 临时工作目录  |

当前配置可支持：

```text
cpp17
cpp20
c11
python3
java17
rust
go
```

---

## 七、核心执行链路

### 7.1 实时事件链路

```text
judge-api 创建 submission
    ↓
写入 submissions，status=PENDING
    ↓
发布 NATS submission.created
    ↓
judge-worker 收到事件
    ↓
try_claim_submission
    ↓
PENDING -> RUNNING
    ↓
编译运行
    ↓
写入 submission_cases
    ↓
更新 submissions
```

---

### 7.2 PENDING 兜底链路

```text
worker 启动
    ↓
扫描 submissions WHERE status='PENDING'
    ↓
逐个 try_claim_submission
    ↓
抢到则判题
```

运行期间：

```text
每 10 秒扫描一次 PENDING
```

这样即使 NATS 消息丢失，提交也不会永久卡在 `PENDING`。

---

## 八、原子抢任务机制

当前 worker 在判题前必须先执行原子抢任务。

核心 SQL：

```sql
UPDATE submissions
SET status = 'RUNNING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

含义：

```text
只有当前状态仍为 PENDING 的 submission 才能被抢到。
```

如果返回一行：

```text
当前 worker 抢到任务，可以开始判题。
```

如果没有返回：

```text
任务已经被其他 worker 抢走
或任务已经判完
或任务状态不是 PENDING
```

当前 worker 会记录 skip 日志并跳过。

---

## 九、PENDING 扫描机制

### 9.1 启动扫描

worker 启动后会执行：

```sql
SELECT id
FROM submissions
WHERE status = 'PENDING'
ORDER BY id ASC
LIMIT $1;
```

扫描到任务后，会逐个调用：

```text
handle_submission
```

由于 `handle_submission` 内部会先 `try_claim_submission`，所以即使多个 worker 同时启动，也不会重复判题。

---

### 9.2 定时扫描

worker 启动后会创建一个后台任务：

```text
每 10 秒扫描一次 PENDING
```

该机制用于恢复：

```text
NATS 消息丢失
worker 临时离线
judge-api 发布事件失败
旧版本 bug 遗留的 PENDING
```

---

## 十、主要代码职责

### 10.1 main.rs

职责：

```text
读取环境变量
加载 languages.yaml
连接 PostgreSQL
连接 NATS
订阅 submission.created
执行启动 PENDING 扫描
启动定时 PENDING 扫描
处理 NATS 实时事件
```

关键函数：

```text
scan_pending_submissions
```

---

### 10.2 event.rs

职责：

```text
解析 NATS Event
提取 submission_id
```

事件结构示例：

```json
{
  "id": "1780234690461528767",
  "type": "submission.created",
  "producer": "judge-api-service",
  "timestamp": "2026-05-31T13:38:10.461534969Z",
  "payload": {
    "submission_id": 9
  }
}
```

---

### 10.3 db.rs

职责：

```text
读取 submission
读取 problem
读取 test_cases
写入 submission_cases
更新 submissions
标记失败
查询 PENDING submissions
原子抢任务
```

当前新增函数：

```rust
pub async fn list_pending_submission_ids(db: &PgPool, limit: i64) -> Result<Vec<i64>>
```

用于扫描历史 PENDING。

```rust
pub async fn try_claim_submission(db: &PgPool, submission_id: i64) -> Result<bool>
```

用于原子抢任务。

---

### 10.4 judge.rs

职责：

```text
处理单个 submission
编译用户代码
运行测试点
比较输出
生成 JudgeResult
保存评测结果
```

当前 `handle_submission` 的第一步是：

```text
try_claim_submission
```

只有抢到任务后才会继续执行。

---

## 十一、评测流程

单个 submission 的流程：

```text
try_claim_submission
    ↓
load_submission
    ↓
load_problem
    ↓
load_test_cases
    ↓
检查语言是否支持
    ↓
创建临时目录
    ↓
写入源代码
    ↓
编译
    ↓
逐测试点运行
    ↓
比较输出
    ↓
生成 CaseResult
    ↓
生成 JudgeResult
    ↓
save_judge_result
```

---

## 十二、当前判题模式

当前采用 ACM 风格短路评测：

```text
遇到第一个非 ACCEPTED 测试点后停止
```

因此当前适合：

```text
传统 ACM / ICPC 风格题目
```

暂不完整支持：

```text
OI 全测试点计分
NOI 子任务
IOI 反馈策略
捆绑点
启发式得分
```

这些后续需要引入：

```text
scoring module
subtask model
bundle model
checker / scorer abstraction
```

---

## 十三、当前 Verdict

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

---

## 十四、当前已验证结果

当前已经验证：

```text
submission 7 由历史 PENDING 恢复为 ACCEPTED
submission 8 由历史 PENDING 恢复为 ACCEPTED
submission 9 通过 NATS 实时事件完成 ACCEPTED
submission 10 新提交后完成 PENDING -> ACCEPTED
```

也验证过：

```text
WRONG_ANSWER
COMPILE_ERROR
TIME_LIMIT_EXCEEDED
```

均可正常写回数据库。

---

## 十五、0ms 说明

当前 `time_ms` 使用：

```rust
start.elapsed().as_millis() as i32
```

因此极短程序可能显示：

```text
0ms
```

这是算法竞赛系统中正常现象，不需要强制改为 `1ms`。

---

## 十六、编译与启动

### 16.1 本地编译

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

---

### 16.2 Docker 启动

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build judge-worker
docker logs ojos-judge-worker --tail 100
```

预期日志：

```text
judge-worker starting
connected successfully
judge-worker subscribed submission.created
pending submissions found
submission claimed
start judging
judge finished
```

---

## 十七、验收命令

### 17.1 查询最近提交

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

```sql
SELECT id, problem_id, user_id, language, status, score, message
FROM submissions
ORDER BY id DESC
LIMIT 10;
```

预期：

```text
不应长期存在 PENDING
```

---

### 17.2 新提交测试

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
user_id = 1
```

---

## 十八、当前限制

当前 Judge Worker 仍存在以下限制：

```text
用户代码直接运行在 judge-worker 容器内
没有独立沙箱
没有真实内存限制
没有文件化测试数据
没有 Special Judge
没有 checker 插件
没有 scorer 插件
没有交互题支持
没有通信题支持
没有提交答案题支持
没有 OI 子任务 / 捆绑点支持
```

---

## 十九、后续计划

Judge Worker 后续建议开发顺序：

```text
1. 测试数据文件化
2. checker-core
3. special judge
4. subtask / bundle
5. OI scoring
6. runner 隔离容器
7. network none
8. cpu / memory / pids 限制
9. interactive runner
10. communication runner
11. output-only scorer
```

短期最建议进入：

```text
Problem / Dataset 正规化
```

因为题型扩展、赛制扩展、SPJ、OI 子任务都依赖更规范的数据模型。

---

## 二十、当前结论

Judge Worker 当前已经从简单事件消费者升级为具备基础可靠性的判题执行器。

当前系统不再完全依赖 NATS 实时事件，而是具备：

```text
实时事件驱动
数据库兜底扫描
原子抢任务
历史 PENDING 恢复
```

这使 Judge 子系统从 MVP v0.1 提升到了：

```text
Judge Worker Reliability v0.2
```

后续重点应从“任务不会丢”转向：

```text
题目数据模型
评测模式扩展
安全隔离
资源限制
```
