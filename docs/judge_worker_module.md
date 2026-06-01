# OJOS Judge Worker 模块开发文档

## 一、模块定位

`services/judge-worker` 是 OJOS 的判题执行模块。

它使用 Rust 编写，是一个后台任务进程，不是 HTTP 服务，不监听端口，也不对外暴露 API。

Judge Worker 的职责是：

```text
消费判题任务
读取提交记录
读取题目信息
读取测试点
加载语言配置
编译用户代码
运行用户程序
执行测试点
比较输出
记录测试点结果
汇总提交结果
写回 PostgreSQL
确认 Redis Stream 消息
恢复历史 PENDING 任务
```

Judge Worker 是当前 OJOS 中真正执行用户代码的模块。

当前 Judge Worker 已经完成：

```text
Redis Streams 判题任务消费
PostgreSQL 提交读取
PostgreSQL 结果写回
多语言配置
基础编译运行
标准输出比较
PENDING 兜底扫描
数据库原子抢任务
Redis XACK 消息确认
```

但当前 Judge Worker 不是最终安全 Runner。

当前用户代码仍然直接运行在 `judge-worker` 容器内部，因此它只适合：

```text
本地开发
可信环境测试
功能链路验证
MVP 演示
```

不适合：

```text
公网开放
陌生用户提交
正式比赛
高安全隔离需求场景
```

后续必须把当前 Worker 演进为：

```text
judge-worker 调度器
+
runner-core 安全执行器
+
sandbox-provider
+
checker-core
+
dataset-core
```

当前文档描述的是 **Judge Worker Reliability v0.3** 的状态。

---

## 二、当前版本状态

当前 Judge Worker 版本状态：

```text
Judge Worker Reliability v0.3
```

当前 Judge Queue 版本：

```text
Judge Queue Redis Streams v0.3
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
C++ / C / Python / Java / Go / Rust 等语言配置
编译阶段
运行阶段
标准输出比较
测试点结果写入
提交总结果写入
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
CE 正常
TLE 正常
Python3 正常
C++17 正常
submission_cases 正常写入
submissions 正常更新
Redis Stream 实时消费正常
Redis XACK 正常
XPENDING 为 0
历史 PENDING 自动恢复
新提交 PENDING -> ACCEPTED
已经判完的 Stream 消息会 skip 并 ACK
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

负责访问 PostgreSQL，包括读取提交、读取题目、读取测试点、写回结果、原子抢任务、扫描 PENDING。

```text
src/judge.rs
```

负责单个 submission 的具体评测流程，包括 claim、编译、运行、比较输出、保存结果。

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
languages.yaml
系统语言工具链
```

当前不依赖：

```text
NATS
```

当前 Docker 镜像中应安装：

```text
g++
gcc
python3
openjdk
golang-go
rust
```

实际支持哪些语言，由以下文件决定：

```text
services/judge-worker/config/languages.yaml
```

也就是说，Docker 镜像中安装了工具链只是前提，真正对外可用的语言必须在 `languages.yaml` 中声明。

---

## 五、环境变量

Judge Worker 当前使用以下环境变量：

| 环境变量               | 说明                 | 默认值                                                               |
| ------------------ | ------------------ | ----------------------------------------------------------------- |
| `REDIS_URL`        | Redis 地址           | `redis://ojos-redis:6379/0`                                       |
| `DATABASE_URL`     | PostgreSQL 地址      | `postgres://postgres:password@postgres:5432/ojos?sslmode=disable` |
| `LANGUAGES_CONFIG` | 语言配置文件路径           | `config/languages.yaml`                                           |
| `JUDGE_WORKER_ID`  | worker consumer 名称 | 未设置时使用 `HOSTNAME` 或 `judge-worker-local`                          |

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
```

不应再有：

```yaml
NATS_URL: nats://ojos-nats:4222
```

也不应再有：

```yaml
depends_on:
  nats:
    condition: service_started
```

如果 Redis 的 service 名不是 `ojos-redis`，而是 `redis`，则容器内 URL 应写为：

```text
redis://redis:6379/0
```

需要和 `docker-compose.yml` 中的 service 名保持一致。

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
serde_yaml = "..."
tracing = "..."
tracing-subscriber = "..."
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
运行命令
运行参数
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

  cpp20:
    source_file: main.cpp
    exe_file: main
    compile:
      enabled: true
      command: g++
      args:
        - "-std=c++20"
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

推荐支持语言：

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
编译命令和运行命令分开配置
编译超时和运行超时分开处理
不支持的语言返回 UNSUPPORTED_LANGUAGE
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
submission_id 16
created_at    2026-05-31T23:39:20Z
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

之前曾经出现过 worker 因 timeout 退出的问题，当前应已修复为：

```rust
let read_result: redis::RedisResult<StreamReadReply> = redis::cmd("XREADGROUP")
    .arg("GROUP")
    .arg(JUDGE_GROUP)
    .arg(&consumer_name)
    .arg("COUNT")
    .arg(1)
    .arg("BLOCK")
    .arg(5000)
    .arg("STREAMS")
    .arg(JUDGE_STREAM)
    .arg(">")
    .query_async(&mut conn)
    .await;

let reply = match read_result {
    Ok(reply) => reply,

    Err(err) if err.is_timeout() || err.to_string().contains("timed out") => {
        continue;
    }

    Err(err) => {
        error!(
            error = %err,
            "xreadgroup failed"
        );

        tokio::time::sleep(Duration::from_secs(1)).await;
        continue;
    }
};
```

如果当前 Redis crate 版本没有 `err.is_timeout()`，可以只使用：

```rust
Err(err) if err.to_string().contains("timed out") => {
    continue;
}
```

---

## 十四、消息解析

Redis Stream 消息中 `submission_id` 是字符串字段。

Worker 需要从消息 map 中读取：

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
处理 worker 停机期间创建的提交
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
SET status = 'RUNNING', updated_at = NOW()
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
PENDING -> RUNNING -> FINAL
```

这个数据库状态流转。

---

## 二十二、handle_submission 流程

`handle_submission` 是单个提交的核心处理函数。

推荐流程：

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
读取 test_cases
    ↓
检查 language 是否支持
    ↓
创建临时工作目录
    ↓
写入用户代码
    ↓
如果需要编译，执行编译命令
    ↓
编译失败 -> COMPILE_ERROR
    ↓
逐个运行测试点
    ↓
收集 stdout / stderr / exit status / time
    ↓
判断 AC / WA / TLE / RE
    ↓
写入 submission_cases
    ↓
汇总 score / max_time / status
    ↓
更新 submissions
```

如果任何系统级错误导致流程无法正常完成，应调用：

```text
mark_submission_failed
```

将提交标记为：

```text
SYSTEM_ERROR
```

避免 submission 永久停留在 RUNNING 或 PENDING。

---

## 二十三、src/db.rs

`src/db.rs` 是数据库访问层。

推荐包含以下结构：

```rust
pub struct Submission {
    pub id: i64,
    pub problem_id: i64,
    pub user_id: i64,
    pub language: String,
    pub code: String,
}

pub struct Problem {
    pub id: i64,
    pub time_limit_ms: i32,
    pub memory_limit_mb: i32,
}

pub struct TestCase {
    pub id: i64,
    pub input: String,
    pub output: String,
    pub score: i32,
}
```

如果编译时提示：

```text
fields id and user_id are never read
```

这只是 Rust dead_code warning，不影响功能。

当前可以接受。

后续如果想消除 warning，可以：

```text
实际使用这些字段
或允许 dead_code
或精简结构体字段
```

但不急。

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
pub async fn load_test_cases(
    db: &PgPool,
    problem_id: i64,
) -> anyhow::Result<Vec<TestCase>>
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
pub async fn save_judge_result(
    db: &PgPool,
    submission_id: i64,
    results: ...
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

之前曾经出现过：

```text
self parameter is only allowed in associated functions
```

原因就是在自由函数中写了：

```rust
pub async fn list_pending_submission_ids(&self, limit: i64)
```

正确写法应是：

```rust
pub async fn list_pending_submission_ids(db: &PgPool, limit: i64)
```

---

## 二十五、src/config.rs

`src/config.rs` 负责读取 `languages.yaml`。

推荐结构：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct LanguagesConfig {
    pub languages: HashMap<String, LanguageConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LanguageConfig {
    pub source_file: String,
    pub exe_file: String,
    pub compile: CompileConfig,
    pub run: RunConfig,
}
```

编译配置：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct CompileConfig {
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}
```

运行配置：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct RunConfig {
    pub command: String,
    pub args: Vec<String>,
}
```

加载函数：

```rust
impl LanguagesConfig {
    pub async fn load(path: &str) -> anyhow::Result<Self> {
        ...
    }
}
```

加载失败时，worker 应启动失败。

原因：

```text
没有语言配置就无法判题
继续运行只会制造 SYSTEM_ERROR
```

---

## 二十六、src/judge.rs

`src/judge.rs` 负责具体评测。

核心函数：

```rust
pub async fn handle_submission(
    db: &PgPool,
    languages: Arc<LanguagesConfig>,
    submission_id: i64,
) -> anyhow::Result<()>
```

流程：

```text
try_claim_submission
    ↓
load_submission
    ↓
load_problem
    ↓
load_test_cases
    ↓
find language config
    ↓
prepare workdir
    ↓
write source file
    ↓
compile if enabled
    ↓
run test cases
    ↓
compare output
    ↓
save results
```

如果语言不存在：

```text
UNSUPPORTED_LANGUAGE
```

如果编译失败：

```text
COMPILE_ERROR
```

如果运行超时：

```text
TIME_LIMIT_EXCEEDED
```

如果输出不匹配：

```text
WRONG_ANSWER
```

如果全部通过：

```text
ACCEPTED
```

如果系统错误：

```text
SYSTEM_ERROR
```

---

## 二十七、编译阶段

对于需要编译的语言：

```text
cpp17
cpp20
c11
java17
go
rust
```

Worker 应执行 `languages.yaml` 中的 compile 配置。

编译参数中的占位符：

```text
{source}
{exe}
{workdir}
```

需要替换为实际路径。

编译超时使用：

```text
compile.timeout_ms
```

如果编译进程超时，可以返回：

```text
COMPILE_ERROR
```

或者后续细分为：

```text
COMPILE_TIMEOUT
```

当前 MVP 使用 `COMPILE_ERROR` 可以接受。

编译失败时，应记录：

```text
stderr
exit code
```

并写入 `submissions.message` 或相关字段。

---

## 二十八、运行阶段

对于每个测试点，Worker 应执行 run 配置。

运行时：

```text
向 stdin 写入 test_case.input
收集 stdout
收集 stderr
等待进程退出
记录耗时
处理超时
处理非零 exit code
```

如果超时：

```text
TIME_LIMIT_EXCEEDED
```

如果非零退出：

```text
RUNTIME_ERROR
```

如果正常退出但输出不匹配：

```text
WRONG_ANSWER
```

如果输出匹配：

```text
ACCEPTED
```

当前时间限制来自：

```text
problems.time_limit_ms
```

当前内存限制字段：

```text
problems.memory_limit_mb
```

尚未真实使用。

---

## 二十九、输出比较

当前 MVP 使用标准输出比较。

推荐比较规则：

```text
去除末尾空白
统一换行差异
整体字符串比较
```

也就是类似：

```text
trim_end(stdout) == trim_end(expected)
```

当前不支持：

```text
Special Judge
浮点误差
多答案
忽略空白 checker
交互 checker
提交答案 checker
```

后续应抽象为：

```text
checker-core
```

并支持：

```text
checker-standard
checker-special
checker-float
checker-output-only
checker-interactive
```

---

## 三十、结果汇总

当前每个测试点会写入：

```text
submission_cases
```

总结果写入：

```text
submissions
```

汇总规则可以是：

```text
如果编译失败 -> COMPILE_ERROR
如果任一测试点 TLE -> TIME_LIMIT_EXCEEDED
如果任一测试点 RE -> RUNTIME_ERROR
如果任一测试点 WA -> WRONG_ANSWER
否则 -> ACCEPTED
```

当前 score 可以简单累加通过测试点分数。

当前传统题如果全部通过：

```text
score = 100
status = ACCEPTED
```

如果部分通过：

```text
status = WRONG_ANSWER
score = 已通过测试点分数
```

后续 OI / IOI / 子任务 / 捆绑点需要更复杂 scoring-core。

当前不要把复杂赛制规则硬编码到 judge-worker 中。

---

## 三十一、状态流转

当前 submission 状态流转：

```text
PENDING
    ↓
RUNNING
    ↓
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
```

`PENDING -> RUNNING` 必须通过原子 SQL 完成。

最终状态应只写一次。

如果评测过程中出现系统错误，应尽量把 RUNNING 改成：

```text
SYSTEM_ERROR
```

避免永久 RUNNING。

---

## 三十二、0ms 与 memory_kb=0

### 32.1 0ms

运行时间显示为：

```text
0ms
```

是正常现象。

对于非常小的程序和小数据，测量结果可能低于 1ms。

算法竞赛系统中 0ms 很常见，不需要强制改成 1ms。

---

### 32.2 memory_kb=0

当前 `memory_kb=0` 是已知限制。

原因：

```text
当前没有 sandbox / cgroup / runner report
```

后续 Runner Core 完成后，应统计真实内存峰值。

当前不要伪造 memory_kb。

---

## 三十三、日志设计

Judge Worker 当前使用：

```text
tracing
tracing_subscriber
JSON 日志
```

推荐重要日志：

```text
worker starting
connected redis successfully
redis stream consumer group created / already exists
judge-worker consuming redis stream
pending submissions found
received judge stream message
submission claimed
submission skipped because it is not pending
start judging
compile failed
test case finished
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
received judge stream message submission_id=16 message_id=...
submission claimed submission_id=16
start judging submission_id=16
judge finished submission_id=16 status=ACCEPTED score=100
judge stream message acked acked=1
```

不建议高频打印：

```text
no pending submissions found
```

否则每 10 秒一条，会刷屏。

---

## 三十四、Redis 调试命令

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

当前正常状态：

```text
XPENDING = 0
```

如果 `XPENDING` 不为 0，说明存在未确认消息。

---

## 三十五、PostgreSQL 调试命令

进入数据库：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

查看最近提交：

```sql
SELECT id, problem_id, user_id, language, status, score, time_ms, memory_kb, message, created_at, updated_at
FROM submissions
ORDER BY id DESC
LIMIT 20;
```

查看某个提交测试点：

```sql
SELECT id, submission_id, test_case_id, status, time_ms, memory_kb, message
FROM submission_cases
WHERE submission_id = 16
ORDER BY id;
```

查看 PENDING：

```sql
SELECT id, problem_id, user_id, language, status, created_at, updated_at
FROM submissions
WHERE status = 'PENDING'
ORDER BY id;
```

查看 RUNNING：

```sql
SELECT id, problem_id, user_id, language, status, created_at, updated_at
FROM submissions
WHERE status = 'RUNNING'
ORDER BY id;
```

手动恢复卡住的 RUNNING，开发环境可用：

```sql
UPDATE submissions
SET status = 'PENDING', updated_at = NOW()
WHERE id = 16 AND status = 'RUNNING';
```

生产环境不应随意手动改状态，需要审计和重测机制。

---

## 三十六、验收流程

### 36.1 编译 worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

预期：

```text
Finished dev profile
```

允许出现 dead_code warning，但不应有 error。

---

### 36.2 重建 Docker

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build judge-worker
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

### 36.3 提交代码

通过 Gateway 登录后提交：

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

### 36.4 查看 Worker 日志

```powershell
docker logs ojos-judge-worker --tail 100
```

预期：

```text
received judge stream message
submission claimed
start judging
judge finished
judge stream message acked
```

---

### 36.5 查询结果

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

### 36.6 检查 Redis Pending

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

预期：

```text
0
```

---

## 三十七、常见问题

### 37.1 提交一直 PENDING

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

### 37.2 worker 启动后退出

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

### 37.3 submission skipped because it is not pending

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

### 37.4 XPENDING 不为 0

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

### 37.5 cargo build 报 self parameter 错误

错误：

```text
self parameter is only allowed in associated functions
```

原因：

```text
在自由函数中写了 &self
```

错误写法：

```rust
pub async fn list_pending_submission_ids(&self, limit: i64)
```

正确写法：

```rust
pub async fn list_pending_submission_ids(db: &PgPool, limit: i64)
```

除非函数在 `impl SomeStruct` 块里，否则不能有 `self` 参数。

---

### 37.6 Redis from_redis_value 类型错误

错误可能是：

```text
expected Value, found &Value
```

解决：

```rust
let text: String = redis::from_redis_value(value.clone()).ok()?;
```

而不是：

```rust
redis::from_redis_value(value)
```

这是 redis crate 版本差异导致的。

---

### 37.7 memory_kb 总是 0

这是当前已知限制。

原因：

```text
没有 sandbox / cgroup 统计
```

后续 Runner Core 实现后解决。

---

### 37.8 0ms 是否要改

不需要。

0ms 在算法竞赛中正常。

---

### 37.9 编译失败但状态不是 COMPILE_ERROR

排查：

```text
compile command 是否正确
compile args 占位符是否替换
workdir 是否正确
source 文件是否写入
stderr 是否捕获
编译失败路径是否调用 mark_submission_failed 或 save result
```

---

### 37.10 Python TLE 或 RE

排查：

```text
python3 是否在 Docker 镜像中
run.command 是否是 python3
args 是否包含 {source}
测试点 input 是否正确传入 stdin
time_limit_ms 是否过小
```

---

## 三十八、安全限制

当前 Judge Worker 最大风险：

```text
用户代码直接运行在 judge-worker 容器内
```

这意味着恶意代码可能：

```text
读取容器文件
写大量文件
占用 CPU
占用内存
创建大量子进程
访问网络
影响其他评测任务
阻塞 worker
```

当前必须避免把该 Worker 暴露给不可信用户。

后续必须实现：

```text
Runner Core
Sandbox Provider
独立运行容器
network none
只读文件系统
临时工作目录隔离
CPU 限制
内存限制
pids 限制
进程超时强杀
输出大小限制
stderr 大小限制
文件大小限制
```

当前 Worker 可以视为：

```text
调度器 + 简易执行器
```

后续应演进为：

```text
调度器 + 安全 runner
```

---

## 三十九、后续架构演进

当前 Judge Worker 负责太多事情：

```text
消费任务
抢任务
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

### 39.1 Runner Core

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

### 39.2 Checker Core

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

### 39.3 Dataset Core

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

当前数据库 TEXT 测试点只是 MVP。

---

### 39.4 Scoring Core

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

## 四十、下一阶段建议

Judge Worker 下一阶段推荐顺序：

```text
1. 降低无 PENDING 时的日志噪声
2. 补充 XAUTOCLAIM 处理长时间 pending 的 Redis 消息
3. 加入 Redis Stream XTRIM
4. 统一 SYSTEM_ERROR / RUNNING 卡死恢复策略
5. 抽象 Runner Core 接口
6. 引入最小容器级 sandbox
7. 实现真实 memory limit
8. 测试数据文件化
9. 抽象 checker-standard
10. 支持 Special Judge
11. 支持子任务 / 捆绑点
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

## 四十一、当前结论

Judge Worker 当前已经完成 OJOS 判题链路中最关键的执行闭环。

它已经从旧的：

```text
NATS Core Pub/Sub 实时事件消费
```

升级为：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 兜底扫描
+
数据库原子抢任务
+
Redis XACK
```

当前 Worker 可以可靠完成：

```text
任务消费
历史恢复
防重复判题
基础编译运行
基础输出比较
结果写回
```

当前 Worker 仍然缺少：

```text
安全沙箱
真实内存统计
真实资源限制
SPJ
子任务
捆绑点
交互题
通信题
提交答案题
```

因此当前 Worker 是：

```text
可靠性 MVP Worker
```

而不是：

```text
生产级安全 Runner
```

后续最重要的方向是：

```text
Runner Core 抽象
安全隔离
测试数据文件化
Checker 抽象
Scoring 抽象
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
