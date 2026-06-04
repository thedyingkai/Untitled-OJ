# Judge Worker 文档

## 一、模块定位

`services/judge-worker` 是 OJOS 的判题执行模块。

它使用 Rust 编写，是一个后台任务进程，不是 HTTP 服务，不监听业务端口，也不对外暴露 API。

Judge Worker 的职责是：

```text
消费判题任务
读取提交记录
读取题目包
读取测试点清单
加载语言配置
使用 nsjail 编译用户代码
使用 nsjail 运行用户程序
执行 checker
执行 scorer
写入 case 输出与日志
写入 result.json
更新 submissions 摘要
确认 Redis Stream 消息
恢复历史 PENDING 任务
```

当前 Judge Worker 已经完成：

```text
Redis Streams 判题任务消费
PostgreSQL 提交读取
PostgreSQL 结果摘要写回
Problem Package 读取
tests/cases.yaml 读取
多语言配置加载
nsjail 编译
nsjail 逐 case 运行
default-trim-checker
default-sum-scorer
submission 文件化存储
result.json 写入
PENDING 兜底扫描
数据库原子抢任务
Redis XACK 消息确认
```

当前 Judge Worker 是：

```text
Package-based nsjail Judge Worker
```

不是最终完整生产级 Runner Core。

当前仍然需要后续继续完善：

```text
memory_kb cgroup v2 峰值统计
输出大小限制
多语言逐项验收
checker 插件化
runner 插件化
scorer 插件化
交互题 / 通信题 / 提交答案题
```

---

## 二、当前版本状态

当前 Judge Worker 可以记为：

```text
Judge Worker v0.4
Judge Queue Redis Streams
Package-based Judge Pipeline
nsjail Sandbox Pipeline
```

当前已经完成：

```text
Rust tokio 异步运行
PostgreSQL 连接
Redis 连接
Redis Stream Consumer Group 初始化
Redis XREADGROUP 消费
Redis XACK 确认
Redis 超时不退出
多语言配置加载
题目包 problem.yaml 读取
测试点 tests/cases.yaml 读取
编译阶段 nsjail 隔离
运行阶段 nsjail 隔离
用户程序 uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
default-trim-checker
default-sum-scorer
result.json 写入
submissions 摘要更新
启动扫描 PENDING
定时扫描 PENDING
原子抢任务
历史 PENDING 恢复
重复任务跳过
异常任务标记 SYSTEM_ERROR
```

当前已经验证：

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
Redis Stream 实时消费正常
Redis XACK 正常
```

当前已经移除：

```text
NATS
NATS_URL
async-nats
futures-util
src/event.rs
submission.created NATS 订阅
旧 Event JSON 解析链路
```

当前 Judge Worker 不应该再出现：

```text
nats
NATS
async_nats
async-nats
NATS_URL
nats://ojos-nats:4222
```

如果仍然存在这些内容，说明 NATS 清理不完整。

---

## 三、模块目录结构

当前 `judge-worker` 推荐目录结构：

```text
services/judge-worker/

├── config/
│   └── languages.yaml
│
├── src/
│   ├── checker.rs
│   ├── config.rs
│   ├── db.rs
│   ├── judge.rs
│   ├── main.rs
│   ├── problem_package.rs
│   ├── result.rs
│   └── sandbox.rs
│
├── Cargo.toml
├── Cargo.lock
└── Dockerfile
```

其中：

```text
config/languages.yaml
```

用于配置语言编译运行方式。

```text
src/config.rs
```

负责读取和解析 `languages.yaml`。

```text
src/db.rs
```

负责访问 PostgreSQL，包括读取提交、读取题目、更新提交摘要、原子抢任务、扫描 PENDING。

```text
src/problem_package.rs
```

负责读取题目包，包括 `problem.yaml` 和 `tests/cases.yaml`。

```text
src/sandbox.rs
```

负责封装 nsjail 编译和运行。

```text
src/checker.rs
```

负责默认输出比较逻辑。

```text
src/result.rs
```

负责 result.json 的结构定义和序列化。

```text
src/judge.rs
```

负责单个 submission 的完整评测流程，包括 claim、加载题目包、编译、运行、checker、scorer、保存结果。

```text
src/main.rs
```

负责进程启动、连接 Redis、连接 PostgreSQL、初始化 Consumer Group、启动 PENDING 扫描任务、循环消费 Redis Stream。

当前不应再存在：

```text
src/event.rs
```

该文件属于旧 NATS 事件解析链路，迁移 Redis Streams 后已经不需要。

---

## 四、运行环境

Judge Worker 通过 Docker Compose 启动。

依赖：

```text
PostgreSQL
Redis
storage/problems
storage/submissions
languages.yaml
nsjail
系统语言工具链
```

当前不依赖：

```text
NATS
```

当前 Docker 镜像中应安装或提供：

```text
nsjail
bash
coreutils
g++
gcc
python3
openjdk-17-jdk
```

后续如果需要支持更多语言，再补充：

```text
golang-go
rustc / cargo
nodejs
```

实际支持哪些语言，由以下文件决定：

```text
services/judge-worker/config/languages.yaml
```

也就是说，Docker 镜像中安装工具链只是前提，真正对外可用的语言必须在 `languages.yaml` 中声明。

---

## 五、环境变量

Judge Worker 当前使用以下环境变量：

| 环境变量               | 说明                 | 默认值                                                               |
| ------------------ | ------------------ | ----------------------------------------------------------------- |
| `REDIS_URL`        | Redis 地址           | `redis://ojos-redis:6379/0`                                       |
| `DATABASE_URL`     | PostgreSQL 地址      | `postgres://postgres:password@postgres:5432/ojos?sslmode=disable` |
| `LANGUAGES_CONFIG` | 语言配置文件路径           | `config/languages.yaml`                                           |
| `JUDGE_WORKER_ID`  | worker consumer 名称 | 未设置时使用 `HOSTNAME` 或 fallback                                      |

当前不再使用：

| 环境变量       | 状态  |
| ---------- | --- |
| `NATS_URL` | 已删除 |

如果 Compose 或 Dockerfile 中仍有：

```text
NATS_URL=nats://ojos-nats:4222
```

应删除。

---

## 六、Docker Compose 配置

`judge-worker` 在 Compose 中应类似：

```yaml
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

不应使用：

```yaml
privileged: true
```

也不应再有：

```yaml
NATS_URL: nats://ojos-nats:4222
```

或：

```yaml
depends_on:
  nats:
    condition: service_started
```

说明：

```text
../../storage:/data/ojos
```

用于让 worker 读写：

```text
/data/ojos/problems
/data/ojos/submissions
```

其中：

```text
/data/ojos/problems
```

只在 worker 容器中可见，用户程序的 nsjail 内不可见。

---

## 七、Cargo.toml 依赖

当前 `Cargo.toml` 应包含：

```toml
[dependencies]
anyhow = "..."
tokio = { version = "...", features = ["full"] }
sqlx = { version = "...", features = ["runtime-tokio", "postgres"] }
redis = { version = "...", features = ["tokio-comp"] }
serde = { version = "...", features = ["derive"] }
serde_json = "..."
serde_yaml = "..."
tracing = "..."
tracing-subscriber = "..."
sha2 = "..."
```

当前不应再包含：

```toml
async-nats = "..."
futures-util = "..."
```

如果使用 `cargo remove`：

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo remove async-nats
cargo remove futures-util
```

如果 `cargo remove` 不可用，手动删除 `Cargo.toml` 中对应依赖，然后执行：

```powershell
cargo build
```

注意：

```text
Cargo.lock 必须提交到 Git
```

因为 `judge-worker` 是应用程序，不是纯库。提交 `Cargo.lock` 可以保证不同环境构建依赖版本一致。

---

## 八、languages.yaml

路径：

```text
services/judge-worker/config/languages.yaml
```

该文件用于配置每种语言的：

```text
源文件名
可执行文件名
是否需要编译
编译命令
编译参数
编译超时
编译内存限制
运行命令
运行参数
```

推荐示例：

```yaml
languages:
  cpp17:
    source_file: main.cpp
    exe_file: main
    compile:
      enabled: true
      command: /usr/bin/g++
      args:
        - "-std=c++17"
        - "-O2"
        - "-pipe"
        - "-B/usr/bin/"
        - "{source}"
        - "-o"
        - "{exe}"
      timeout_ms: 10000
      memory_mb: 2048
    run:
      command: "{exe}"
      args: []

  cpp20:
    source_file: main.cpp
    exe_file: main
    compile:
      enabled: true
      command: /usr/bin/g++
      args:
        - "-std=c++20"
        - "-O2"
        - "-pipe"
        - "-B/usr/bin/"
        - "{source}"
        - "-o"
        - "{exe}"
      timeout_ms: 10000
      memory_mb: 2048
    run:
      command: "{exe}"
      args: []

  c11:
    source_file: main.c
    exe_file: main
    compile:
      enabled: true
      command: /usr/bin/gcc
      args:
        - "-std=c11"
        - "-O2"
        - "-pipe"
        - "-B/usr/bin/"
        - "{source}"
        - "-o"
        - "{exe}"
      timeout_ms: 10000
      memory_mb: 1024
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
      memory_mb: 512
    run:
      command: /usr/bin/python3
      args:
        - "{source}"

  java17:
    source_file: Main.java
    exe_file: ""
    compile:
      enabled: true
      command: /usr/bin/javac
      args:
        - "{source}"
      timeout_ms: 10000
      memory_mb: 2048
    run:
      command: /usr/bin/java
      args:
        - "-cp"
        - "{workdir}"
        - "Main"
```

支持占位符：

| 占位符         | 含义      |
| ----------- | ------- |
| `{source}`  | 源文件路径   |
| `{exe}`     | 可执行文件路径 |
| `{workdir}` | 工作目录    |

语言配置原则：

```text
语言命令不硬编码在 Rust 代码中
新增语言优先修改 languages.yaml
编译命令和运行命令分开配置
编译超时和运行超时分开处理
不支持的语言返回 UNSUPPORTED_LANGUAGE
命令建议使用绝对路径
C/C++ 建议使用 -B/usr/bin/ 保证 ld 可被找到
```

后续可以把语言支持演进为：

```text
language-pack-cpp
language-pack-python
language-pack-java
language-pack-go
language-pack-rust
```

由模块系统安装。

---

## 九、主执行链路

当前 Judge Worker 的主链路如下：

```text
启动进程
    ↓
初始化 tracing_subscriber JSON 日志
    ↓
读取 REDIS_URL
    ↓
读取 DATABASE_URL
    ↓
读取 LANGUAGES_CONFIG
    ↓
确定 consumer_name
    ↓
加载 languages.yaml
    ↓
连接 PostgreSQL
    ↓
连接 Redis
    ↓
PING Redis
    ↓
确保 Redis Consumer Group 存在
    ↓
启动扫描 PENDING
    ↓
启动定时 PENDING 扫描任务
    ↓
循环 XREADGROUP
    ↓
解析 Stream 消息
    ↓
获取 submission_id
    ↓
handle_submission
    ↓
XACK
```

该链路中，最重要的可靠性设计是：

```text
Redis Streams 负责实时任务投递
PostgreSQL PENDING 扫描负责兜底恢复
数据库原子抢任务负责防重复执行
XACK 负责确认消息消费
```

---

## 十、Redis Streams 设计

当前 Judge Queue 使用 Redis Streams。

Stream 名称：

```text
ojos:judge:submissions
```

Consumer Group 名称：

```text
judge-workers
```

消息生产者：

```text
judge-api
```

消息消费者：

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
submission_id 20
created_at    2026-06-04T13:28:02Z
```

---

## 十一、Consumer Group 初始化

Worker 启动时需要确保 Consumer Group 存在。

Redis 命令：

```text
XGROUP CREATE ojos:judge:submissions judge-workers $ MKSTREAM
```

含义：

```text
如果 stream 不存在，MKSTREAM 会创建 stream
如果 group 不存在，则创建 group
如果 group 已存在，则返回 BUSYGROUP
```

Worker 处理规则：

```text
创建成功 -> info 日志
BUSYGROUP -> 说明已存在，正常继续
其他错误 -> 启动失败或记录错误
```

当前日志可能出现：

```text
redis stream consumer group created
```

或：

```text
redis stream consumer group already exists
```

两者都是正常状态。

---

## 十二、XREADGROUP 消费逻辑

Worker 使用 `XREADGROUP` 消费新消息。

命令逻辑：

```text
XREADGROUP
  GROUP judge-workers <consumer-name>
  COUNT 1
  BLOCK 5000
  STREAMS ojos:judge:submissions >
```

其中：

```text
GROUP judge-workers <consumer-name>
```

表示使用 `judge-workers` 这个 consumer group，并以当前 worker 的 consumer name 消费。

```text
COUNT 1
```

表示一次读取一条消息。

```text
BLOCK 5000
```

表示最多阻塞 5 秒等待新消息。

```text
STREAMS ojos:judge:submissions >
```

表示读取从未投递给其他 consumer 的新消息。

---

## 十三、Redis 超时处理

`BLOCK 5000` 在没有新消息时可能返回超时。

这个超时不是错误。

Worker 不能因为 Redis 没有新消息就退出。

正确逻辑：

```text
XREADGROUP timeout
    ↓
continue
    ↓
下一轮继续等待
```

错误逻辑：

```text
XREADGROUP timeout
    ↓
返回 Err
    ↓
main 函数退出
    ↓
worker 停止
    ↓
后续 submission 一直 PENDING
```

如果 Redis crate 版本支持 timeout 判断，可以写：

```rust
Err(err) if err.is_timeout() || err.to_string().contains("timed out") => {
    continue;
}
```

如果没有 `is_timeout()`，可以退化为：

```rust
Err(err) if err.to_string().contains("timed out") => {
    continue;
}
```

其他错误建议：

```text
记录 error
sleep 1 秒
continue
```

不要直接退出 worker 主循环。

---

## 十四、消息解析

Redis Stream 消息中 `submission_id` 是字符串字段。

Worker 需要从 message map 中读取：

```text
submission_id
```

然后解析为整数。

示例函数：

```rust
fn parse_submission_id_from_stream(map: &HashMap<String, redis::Value>) -> Option<i64> {
    let value = map.get("submission_id")?;
    let text: String = redis::from_redis_value(value.clone()).ok()?;
    text.parse::<i64>().ok()
}
```

注意：

```text
某些 redis crate 版本 from_redis_value 需要 Value 而不是 &Value
```

因此这里使用：

```rust
value.clone()
```

如果消息缺失 `submission_id` 或无法解析：

```text
记录 warn
跳过判题
XACK 消息
```

不要让一条坏消息导致 worker 退出。

---

## 十五、XACK 逻辑

Worker 处理完一条 Redis Stream 消息后，需要执行：

```text
XACK ojos:judge:submissions judge-workers <message_id>
```

含义：

```text
确认该消息在 judge-workers consumer group 中已处理
从 Pending Entries List 中移除
```

Rust 逻辑示例：

```rust
async fn ack_stream_message(
    conn: &mut redis::aio::MultiplexedConnection,
    message_id: &str,
) -> Result<()> {
    let acked: i64 = redis::cmd("XACK")
        .arg(JUDGE_STREAM)
        .arg(JUDGE_GROUP)
        .arg(message_id)
        .query_async(conn)
        .await
        .context("xack failed")?;

    info!(
        message_id = %message_id,
        acked,
        "judge stream message acked"
    );

    Ok(())
}
```

注意：

```text
XACK 不删除 Stream 历史消息
XACK 只从 Consumer Group 的 PEL 中确认消息
```

所以执行 `XRANGE` 仍能看到历史消息是正常现象。

---

## 十六、Redis Stream 历史消息

Redis Stream 是 append-only 风格的数据结构。

即使消息已经 XACK：

```text
XRANGE ojos:judge:submissions - +
```

仍然能看到历史消息。

这是正常的。

如果要控制 Stream 长度，后续可以加入：

```text
XTRIM ojos:judge:submissions MAXLEN ~ 10000
```

当前暂时不急。

原因：

```text
当前提交量很小
调试阶段保留历史消息有助于排查
过早 trim 可能影响调试
```

后续如果提交量变大，需要在 judge-api XADD 后或定期任务中加入近似裁剪。

---

## 十七、PENDING 兜底扫描

即使使用 Redis Streams，也必须保留 PENDING 扫描。

原因：

```text
judge-api 可能 INSERT submission 成功但 XADD 失败
Redis 可能短暂不可用
worker 可能消费消息后崩溃
旧版本可能遗留 PENDING
手动修复数据后需要恢复
Redis Stream 消息可能被异常 ACK
```

因此最终可靠模型是：

```text
PostgreSQL 是最终事实来源
Redis Streams 是实时任务队列
PENDING 扫描是兜底恢复机制
try_claim_submission 是防重复执行机制
```

---

## 十八、启动扫描

Worker 启动后应立即扫描 PENDING。

SQL：

```sql
SELECT id
FROM submissions
WHERE status = 'PENDING'
ORDER BY id ASC
LIMIT $1;
```

作用：

```text
恢复历史 PENDING
处理 worker 停机时创建的提交
处理 Redis XADD 失败但 DB 写入成功的提交
处理旧版本遗留提交
```

启动扫描不是临时修补，而是可靠性设计的一部分。

---

## 十九、定时扫描

Worker 运行期间应周期性扫描 PENDING。

当前建议周期：

```text
10 秒
```

逻辑：

```text
每 10 秒查询一批 PENDING
如果没有任务，直接返回
如果有任务，逐个 handle_submission
```

建议不要在没有任务时频繁打印：

```text
no pending submissions found
```

否则日志会刷屏。

推荐：

```rust
if ids.is_empty() {
    return Ok(());
}
```

只在发现任务时打印：

```rust
info!(
    count = ids.len(),
    "pending submissions found"
);
```

---

## 二十、数据库原子抢任务

Worker 在真正判题前必须先执行原子抢任务。

核心 SQL：

```sql
UPDATE submissions
SET status = 'JUDGING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

含义：

```text
只有当前仍然是 PENDING 的 submission 才能被抢到
```

如果返回一行：

```text
当前 worker 抢到任务
可以开始判题
```

如果返回空：

```text
任务已经被其他 worker 抢走
或任务已经判完
或任务状态不是 PENDING
```

此时 worker 应记录 skip 日志并跳过。

这条 SQL 是防重复判题的核心。

Redis Streams 可以保证任务投递可靠性，但不能替代数据库状态机。

---

## 二十一、为什么 Redis 不能替代数据库状态机

Redis Streams 负责：

```text
任务投递
任务积压
Consumer Group
Pending Entries List
ACK
```

数据库状态机负责：

```text
submission 当前真实状态
防止重复判题
最终结果持久化
用户查询
系统事实来源
```

如果只依赖 Redis，可能出现：

```text
两个 worker 都拿到同一个 submission
worker 启动扫描和 Redis 实时消息重复处理
旧消息重复投递
手动重测与旧任务冲突
```

因此必须保留：

```text
PENDING -> JUDGING -> FINAL
```

这个数据库状态流转。

---

## 二十二、handle_submission 流程

`handle_submission` 是单个提交的核心处理函数。

当前流程：

```text
接收 submission_id
    ↓
try_claim_submission
    ↓
抢不到则 skip
    ↓
读取 submission
    ↓
读取 problem
    ↓
读取 problems.package_dir
    ↓
读取 problem.yaml
    ↓
读取 tests/cases.yaml
    ↓
检查 language 是否支持
    ↓
准备 submission 目录
    ↓
复制用户源码到 build 目录
    ↓
如果需要编译，使用 nsjail 编译
    ↓
编译失败 -> COMPILE_ERROR
    ↓
逐个 case 创建独立目录
    ↓
复制 input 到 stdin.txt
    ↓
复制可执行文件到 case 目录
    ↓
使用 nsjail 运行用户程序
    ↓
收集 stdout.txt / stderr.txt / exit status / time
    ↓
读取 answer
    ↓
default-trim-checker
    ↓
写 checker.log
    ↓
生成 case result
    ↓
default-sum-scorer 汇总
    ↓
写 result.json
    ↓
更新 submissions 摘要
```

如果任何系统级错误导致流程无法正常完成，应调用失败标记逻辑，将提交标记为：

```text
SYSTEM_ERROR
```

避免 submission 永久停留在 `JUDGING` 或 `PENDING`。

---

## 二十三、db.rs 职责

`src/db.rs` 是数据库访问层。

当前推荐核心结构：

```rust
pub struct Submission {
    pub id: i64,
    pub problem_id: i64,
    pub user_id: i64,
    pub language: String,
    pub code_path: String,
    pub result_path: String,
}

pub struct Problem {
    pub id: i64,
    pub package_dir: String,
    pub time_limit_ms: i32,
    pub memory_limit_mb: i32,
}
```

不应再使用：

```rust
pub struct TestCase {
    pub id: i64,
    pub input: String,
    pub output: String,
    pub score: i32,
}
```

因为测试点已经来自：

```text
tests/cases.yaml
```

而不是数据库 `test_cases`。

---

## 二十四、db.rs 推荐函数

`db.rs` 推荐提供：

```rust
pub async fn load_submission(
    db: &PgPool,
    submission_id: i64,
) -> anyhow::Result<Submission>
```

```rust
pub async fn load_problem(
    db: &PgPool,
    problem_id: i64,
) -> anyhow::Result<Problem>
```

```rust
pub async fn try_claim_submission(
    db: &PgPool,
    submission_id: i64,
) -> anyhow::Result<bool>
```

```rust
pub async fn list_pending_submission_ids(
    db: &PgPool,
    limit: i64,
) -> anyhow::Result<Vec<i64>>
```

```rust
pub async fn update_submission_summary(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    score: i32,
    time_ms: i32,
    memory_kb: i32,
    message: &str,
) -> anyhow::Result<()>
```

```rust
pub async fn mark_submission_failed(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    message: &str,
) -> anyhow::Result<()>
```

注意：

```text
list_pending_submission_ids 和 try_claim_submission 是普通函数
不要写成带 &self 的函数，除非它们在 impl 块里
```

错误写法：

```rust
pub async fn list_pending_submission_ids(&self, limit: i64)
```

正确写法：

```rust
pub async fn list_pending_submission_ids(db: &PgPool, limit: i64)
```

---

## 二十五、problem_package.rs 职责

`src/problem_package.rs` 负责读取题目包。

它需要读取：

```text
problem.yaml
tests/cases.yaml
```

核心结构建议：

```rust
pub struct ProblemManifest {
    pub schema: String,
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub limits: Limits,
    pub tests: TestsConfig,
    pub runner: RunnerConfig,
    pub checker: CheckerConfig,
    pub scorer: ScorerConfig,
}
```

```rust
pub struct TestsConfig {
    pub root: String,
    pub cases: String,
    pub groups: String,
}
```

```rust
pub struct CaseManifest {
    pub case_no: i32,
    pub input: String,
    pub answer: String,
    pub score: i32,
    pub group: i32,
    pub sample: bool,
    pub hidden: bool,
}
```

路径规则：

```text
package_dir + problem.yaml
package_dir + tests.cases
package_dir + tests.root + case.input
package_dir + tests.root + case.answer
```

注意：

```text
tests.cases 是相对 package_dir 的路径，例如 tests/cases.yaml
case.input / case.answer 是相对 tests.root 的路径
```

不要把：

```text
tests.root + tests.cases
```

再拼一次，否则会变成：

```text
tests/tests/cases.yaml
```

旧格式：

```yaml
cases:
  - "no": 0
```

不再兼容。

当前要求：

```yaml
cases:
  - case_no: 1
```

---

## 二十六、sandbox.rs 职责

`src/sandbox.rs` 负责封装 nsjail。

它需要支持：

```text
编译命令执行
运行命令执行
超时控制
地址空间限制
stdin/stdout/stderr 文件重定向
exit code 解析
TLE / RE / MLE 状态识别
```

当前 nsjail 基础参数类似：

```text
nsjail
  --mode o
  --user 10001
  --group 10001
  --disable_clone_newuser
  --time_limit <sec>
  --rlimit_as <memory_mb>
  --rlimit_nofile 64
  --rlimit_nproc 64
  --cwd /work
  --chroot /jail/root
  --bindmount_ro /bin:/bin
  --bindmount_ro /lib:/lib
  --bindmount_ro /lib64:/lib64
  --bindmount_ro /usr:/usr
  --bindmount_ro /etc/alternatives:/etc/alternatives
  --bindmount_ro /dev/null:/dev/null
  --bindmount_ro /dev/zero:/dev/zero
  --bindmount_ro /dev/urandom:/dev/urandom
  --bindmount <case_or_build_dir>:/work
  --tmpfsmount /tmp
  --
  /bin/bash -lc "<command>"
```

注意：

```text
所有 nsjail 参数必须放在 -- 前面
-- 后面才是真正执行的命令
```

错误写法：

```text
nsjail ... -- /bin/bash -lc "<command>" --user 10001
```

这种情况下 `--user 10001` 已经不是 nsjail 参数，而是 bash 参数。

---

## 二十七、编译阶段

对于需要编译的语言，例如：

```text
cpp17
cpp20
c11
java17
```

Worker 应执行 `languages.yaml` 中的 compile 配置。

编译工作目录：

```text
storage/submissions/{id}/build/
```

编译日志：

```text
storage/submissions/{id}/build/compile.log
storage/submissions/{id}/build/compile.stdout.log
storage/submissions/{id}/build/compile.stderr.log
```

编译命令在 jail 内执行时，应使用 `/work` 路径。

例如 C++：

```text
/usr/bin/g++ -std=c++17 -O2 -pipe -B/usr/bin/ /work/main.cpp -o /work/main
```

编译日志应通过 jail 内重定向写入：

```text
/work/compile.stdout.log
/work/compile.stderr.log
```

不要依赖父进程 FD 捕获，否则在某些场景下日志可能为空。

编译失败时：

```text
status = COMPILE_ERROR
message = compile.log 中的摘要
cases = []
```

---

## 二十八、运行阶段

对于每个测试点，Worker 应创建独立 case 目录：

```text
storage/submissions/{id}/cases/{case_no:03}/
```

例如：

```text
storage/submissions/20/cases/001/
```

每个 case 目录包含：

```text
stdin.txt
stdout.txt
stderr.txt
checker.log
```

运行时应在 jail 内执行：

```text
/work/main < /work/stdin.txt > /work/stdout.txt 2> /work/stderr.txt
```

也就是说：

```text
stdin/stdout/stderr 通过 /work 内文件重定向
```

不要依赖父进程 FD 传递。

这样可以避免：

```text
stdout 为空
stderr 捕获不稳定
Windows bind mount + nsjail FD 行为异常
```

每次运行 case 前，应删除旧文件：

```text
stdout.txt
stderr.txt
checker.log
```

否则旧文件可能属于 root 且权限为 `0644`，用户程序 `uid=10001` 无法截断，导致 bash 在重定向阶段直接失败。

---

## 二十九、Checker

当前默认 checker：

```text
default-trim-checker
```

规则：

```text
统一 CRLF / CR 为 LF
去除每行末尾空格和 Tab
去除末尾空行
不忽略行内空格
不忽略多余非空行
```

示例：

```text
actual:   "3\n"
expected: "3\n"
=> AC

actual:   "3   \n\n"
expected: "3\n"
=> AC

actual:   "3 4\n"
expected: "3\n"
=> WA
```

WA 时 `checker.log` 应写入类似：

```text
expected: 3
actual: 0
```

AC 时 `checker.log` 应写入：

```text
accepted
```

后续需要扩展：

```text
special judge
float checker
ignore whitespace checker
interactive checker
output-only checker
```

---

## 三十、Scorer

当前默认 scorer：

```text
default-sum-scorer
```

规则：

```text
每个测试点 AC 得该测试点分数
非 AC 得 0
总分为所有测试点得分之和
全部测试点 AC -> ACCEPTED
存在 WA -> WRONG_ANSWER
存在 TLE -> TIME_LIMIT_EXCEEDED
存在 RE -> RUNTIME_ERROR
存在 SYSTEM_ERROR -> SYSTEM_ERROR
```

当前传统题如果全部通过：

```text
score = 100
status = ACCEPTED
```

如果部分通过：

```text
score = 已通过测试点分数
status = WRONG_ANSWER 或其他失败状态
```

后续 OI / IOI / 子任务 / 捆绑点需要更复杂的 scoring-core。

当前不要把复杂赛制规则硬编码到 `judge.rs` 中。

---

## 三十一、结果写入

当前完整结果写入：

```text
storage/submissions/{id}/result.json
```

示例：

```json
{
  "submission_id": 20,
  "status": "ACCEPTED",
  "score": 100,
  "time_ms": 21,
  "memory_kb": 0,
  "message": "",
  "cases": [
    {
      "case_no": 1,
      "status": "ACCEPTED",
      "score": 100,
      "time_ms": 21,
      "memory_kb": 0,
      "stdout_path": "/data/ojos/submissions/20/cases/001/stdout.txt",
      "stderr_path": "/data/ojos/submissions/20/cases/001/stderr.txt",
      "checker_log_path": "/data/ojos/submissions/20/cases/001/checker.log",
      "message": ""
    }
  ]
}
```

数据库 `submissions` 表只更新摘要：

```text
status
score
time_ms
memory_kb
message
result_path
judged_at
updated_at
```

当前不再写：

```text
submission_cases
```

---

## 三十二、状态流转

当前 submission 状态流转：

```text
PENDING
    ↓
JUDGING
    ↓
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
```

`PENDING -> JUDGING` 必须通过原子 SQL 完成。

最终状态应只写一次。

如果评测过程中出现系统错误，应尽量把 `JUDGING` 改成：

```text
SYSTEM_ERROR
```

避免永久 `JUDGING`。

Cancel 和 Rejudge 由 `judge-api` 负责触发：

```text
CANCELLED
```

和：

```text
PENDING
```

Worker 只处理进入 `PENDING` 的任务。

---

## 三十三、时间与内存

### 33.1 time_ms

`time_ms` 记录运行耗时。

对于非常小的程序和小数据，测量结果可能低于 1ms。

算法竞赛系统中：

```text
0ms
```

是正常现象，不需要强制改成 1ms。

---

### 33.2 memory_kb

当前：

```text
memory_kb = 0
```

是已知限制。

原因：

```text
当前只做了内存限制，没有做峰值内存采集
```

当前内存限制主要依赖：

```text
nsjail --rlimit_as
```

后续应通过：

```text
cgroup v2
```

统计真实峰值内存，并写入：

```text
case.memory_kb
submission.memory_kb
```

当前不要伪造 memory_kb。

---

## 三十四、日志设计

Judge Worker 当前使用：

```text
tracing
tracing_subscriber
JSON 日志
```

推荐重要日志：

```text
judge-worker starting
connected redis successfully
redis stream consumer group created / already exists
judge-worker consuming redis stream
pending submissions found
received judge stream message
submission claimed
submission skipped because it is not pending
judge finished
judge stream message acked
judge submission failed
```

推荐字段：

```text
submission_id
problem_id
language
message_id
consumer
stream
group
status
time_ms
error
```

示例：

```text
received judge stream message submission_id=20 message_id=...
submission claimed submission_id=20
judge finished submission_id=20 status=ACCEPTED score=100
judge stream message acked acked=1
```

不建议高频打印：

```text
no pending submissions found
```

否则每 10 秒一条，会刷屏。

---

## 三十五、Redis 调试命令

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

查看长度：

```powershell
docker exec -it ojos-redis redis-cli XLEN ojos:judge:submissions
```

手动裁剪：

```powershell
docker exec -it ojos-redis redis-cli XTRIM ojos:judge:submissions MAXLEN "~" 10000
```

当前正常状态通常是：

```text
XPENDING = 0
```

如果 `XPENDING` 不为 0，说明存在未确认消息。

---

## 三十六、PostgreSQL 调试命令

进入数据库：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
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
    created_at,
    updated_at,
    judged_at
FROM submissions
ORDER BY id DESC
LIMIT 20;
```

查看 PENDING：

```sql
SELECT id, problem_id, user_id, language, status, created_at, updated_at
FROM submissions
WHERE status = 'PENDING'
ORDER BY id;
```

查看 JUDGING：

```sql
SELECT id, problem_id, user_id, language, status, created_at, updated_at
FROM submissions
WHERE status = 'JUDGING'
ORDER BY id;
```

开发环境中，手动恢复卡住的 `JUDGING` 可用：

```sql
UPDATE submissions
SET status = 'PENDING', updated_at = NOW()
WHERE id = 20 AND status = 'JUDGING';
```

生产环境不应随意手动改状态，需要审计和重测机制。

---

## 三十七、文件调试命令

查看提交结果：

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\result.json" -Encoding UTF8
```

查看编译日志：

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\build\compile.log" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\build\compile.stdout.log" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\build\compile.stderr.log" -Encoding UTF8
```

查看测试点输出：

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\stdin.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\stdout.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\stderr.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\checker.log" -Encoding UTF8
```

查看题目包：

```powershell
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\problem.yaml" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\tests\cases.yaml" -Encoding UTF8
Get-ChildItem "D:\Untitled-OJ\storage\problems\2-a-plus-b\tests" -Recurse
```

---

## 三十八、验收流程

### 38.1 编译 worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo check
cargo build
```

预期：

```text
Finished dev profile
```

允许出现不影响功能的 warning，但不应有 error。

---

### 38.2 重建 Docker

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose build judge-worker
docker compose up -d judge-worker
```

查看日志：

```powershell
docker logs ojos-judge-worker --tail 100
```

预期看到：

```text
connected redis successfully
redis stream consumer group already exists
judge-worker consuming redis stream
```

或：

```text
redis stream consumer group created
judge-worker consuming redis stream
```

---

### 38.3 提交代码

通过 Gateway 登录后提交：

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
submission_id = 新 ID
status = PENDING
```

---

### 38.4 查看 Worker 日志

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

### 38.5 查询结果

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

---

### 38.6 检查 Redis Pending

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

预期：

```text
0
```

---

## 三十九、常见问题

### 39.1 提交一直 PENDING

排查顺序：

```text
judge-api 是否成功写入 submissions
judge-api 是否成功 XADD Redis Stream
Redis Stream 是否有消息
worker 是否运行
worker 是否连接 Redis
worker 是否连接 PostgreSQL
worker 是否 XREADGROUP 超时后退出
worker 是否启动扫描 PENDING
try_claim_submission 是否成功
handle_submission 是否报错
```

命令：

```powershell
docker logs ojos-judge-api --tail 100
docker logs ojos-judge-worker --tail 100
docker exec -it ojos-redis redis-cli XRANGE ojos:judge:submissions - +
```

数据库：

```sql
SELECT id, status, message
FROM submissions
ORDER BY id DESC
LIMIT 10;
```

---

### 39.2 worker 启动后退出

如果日志出现：

```text
xreadgroup failed: timed out
```

说明 timeout 被当成 fatal error。

解决：

```text
timeout 时 continue
不要 return Err
不要让 main 退出
```

---

### 39.3 submission skipped because it is not pending

这不是错误。

常见原因：

```text
启动扫描先判完了 submission
Redis Stream 中仍有历史消息
worker 之后又读到该消息
try_claim_submission 发现不是 PENDING
于是 skip 并 XACK
```

这是防重复判题正常工作。

---

### 39.4 XPENDING 不为 0

说明某些消息被投递给 consumer，但没有被 ACK。

原因可能是：

```text
worker 消费后崩溃
worker 判题中卡住
worker 没执行 XACK
Redis 连接中断
```

当前可以通过 PENDING 数据库扫描兜底，但 Redis PEL 中仍会有积压。

后续应实现：

```text
XAUTOCLAIM
```

用于回收长时间 pending 的 Redis Stream 消息。

---

### 39.5 load cases.yaml failed

说明 worker 读取题目包失败。

重点检查：

```text
problems.package_dir 是否正确
problem.yaml 是否存在
problem.yaml 中 tests.cases 是否正确
tests/cases.yaml 是否存在
```

注意路径规则：

```text
tests.cases 是相对 package_dir 的路径
case.input / case.answer 是相对 tests.root 的路径
```

如果出现：

```text
tests/tests/cases.yaml
```

说明错误地把 `tests.root` 和 `tests.cases` 重复拼接了。

---

### 39.6 compile error 但 compile.log 为空

应检查编译日志是否在 jail 内重定向到：

```text
/work/compile.stdout.log
/work/compile.stderr.log
```

不要依赖父进程 FD 捕获编译日志。

如果 C++ 编译提示：

```text
collect2: fatal error: cannot find 'ld'
```

应确认：

```text
languages.yaml 使用 /usr/bin/g++
C/C++ compile args 包含 -B/usr/bin/
shell PATH 包含 /usr/bin
```

---

### 39.7 runtime error code 127

`code 127` 通常表示：

```text
command not found
```

常见原因：

```text
languages.yaml 中 run.command = "{exe}"
但 worker 没有对 command 本身做占位符替换
```

应确保 command 和 args 都会进行：

```text
{source}
{exe}
{workdir}
```

替换。

---

### 39.8 stdout.txt 为空但程序应该有输出

排查：

```text
stdin.txt 是否有内容
运行命令是否使用 /work/stdin.txt 重定向
运行命令是否使用 /work/stdout.txt 重定向
stdout.txt 是否旧文件权限导致无法截断
```

每次运行 case 前应删除：

```text
stdout.txt
stderr.txt
checker.log
```

让 `uid=10001` 在 jail 内重新创建。

---

### 39.9 memory_kb 总是 0

这是当前已知限制。

原因：

```text
当前没有 cgroup v2 峰值内存统计
```

后续解决，不要伪造。

---

### 39.10 0ms 是否要改

不需要。

0ms 在算法竞赛中正常。

---

## 四十、安全限制

当前 Worker 已经具备基础 nsjail 隔离，但仍不是最终生产级 Runner。

当前已经做到：

```text
用户程序不以 root 身份运行
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
用户程序不能读取 ans
用户程序不能覆盖题目数据
每个 case 独立运行
```

仍需继续完善：

```text
cgroup v2 memory peak
更严格的 pids 限制
输出大小限制
stderr 大小限制
文件大小限制
系统调用策略
多语言隔离策略
worker 并发控制
恶意编译器行为限制
```

当前 Worker 可以视为：

```text
调度器 + 基础 nsjail 执行器
```

后续应演进为：

```text
调度器 + Runner Core + Sandbox Provider
```

---

## 四十一、后续架构演进

当前 Judge Worker 仍然负责较多事情：

```text
消费任务
抢任务
加载题目包
编译
运行
比较
计分
写结果
```

后续应拆分职责：

```text
judge-worker
    负责任务消费、任务调度、结果写回

runner-core
    负责编译运行和资源限制

checker-core
    负责输出判断

dataset-core
    负责测试数据读取

scoring-core
    负责分数聚合和赛制相关反馈
```

---

### 41.1 Runner Core

Runner Core 应负责：

```text
创建隔离环境
写入源代码
执行编译
执行运行
限制 CPU
限制内存
限制进程数
限制网络
限制文件系统
收集 stdout
收集 stderr
收集 exit code
收集 time
收集 memory
返回原始运行结果
```

Runner Core 不应该负责：

```text
ACM 罚时
OI 子任务
榜单计算
题目权限
用户权限
```

---

### 41.2 Checker Core

Checker Core 应负责：

```text
标准输出比较
Special Judge
浮点误差
忽略空白
交互题检查
提交答案题检查
```

当前的标准输出比较可以演进为：

```text
checker-standard
```

---

### 41.3 Dataset Core

Dataset Core 应负责：

```text
测试数据文件化
测试点 metadata
输入文件
输出文件
子任务
捆绑点
样例
数据包导入
数据权限
数据校验
```

当前题目包是 Dataset Core 的基础形态。

---

### 41.4 Scoring Core

Scoring Core 应负责：

```text
传统题全 AC
OI 部分分
NOI 捆绑点
IOI 反馈策略
ACM 只看 AC / WA
启发式题得分
```

不要把这些复杂规则全部写死在 `judge.rs`。

---

## 四十二、下一阶段建议

Judge Worker 下一阶段推荐顺序：

```text
1. 多语言验收：c11 / python3 / java17
2. cgroup v2 memory peak 统计
3. 输出大小限制
4. XAUTOCLAIM 处理长时间 pending 的 Redis 消息
5. Redis Stream XTRIM
6. 统一 SYSTEM_ERROR / JUDGING 卡死恢复策略
7. 抽象 Runner Core 接口
8. 抽象 checker-standard
9. 支持 Special Judge
10. 支持子任务 / 捆绑点
```

当前不建议立刻把：

```text
交互题
通信题
启发式题
复杂赛制
```

写进 worker。

原因：

```text
Runner Core 还没稳定
Dataset Core 还没稳定
Checker Core 还没稳定
Scoring Core 还没稳定
```

直接加复杂题型会导致后续大规模重构。

---

## 四十三、当前结论

Judge Worker 当前已经完成 OJOS 判题链路中最关键的执行闭环。

它已经从旧的：

```text
NATS Core Pub/Sub 实时事件消费
+
数据库测试点
+
容器内裸跑用户程序
```

升级为：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 兜底扫描
+
数据库原子抢任务
+
Problem Package
+
nsjail Sandbox
+
File-based Result
```

当前 Worker 可以可靠完成：

```text
任务消费
历史恢复
防重复判题
题目包读取
基础沙箱编译运行
基础输出比较
结果落盘
摘要回写
```

当前 Worker 仍然缺少：

```text
真实内存统计
输出大小限制
SPJ
子任务
捆绑点
交互题
通信题
提交答案题
```

因此当前 Worker 是：

```text
Package-based nsjail Judge Worker
```

后续最重要的方向是：

```text
Runner Core 抽象
cgroup v2 内存统计
Checker 抽象
Scoring 抽象
Dataset Core 深化
```

只有这些稳定后，OJOS 才能继续安全地支持：

```text
OI
NOI
ACM
IOI
启发式算法题
交互题
通信题
提交答案题
滚榜
封榜
气球
打印
ICPC Tools 兼容
```
