# OJOS Shared 模块开发文档

## 一、模块定位

`services/shared` 是 OJOS Go 微服务体系中的公共基础设施 SDK。

它本身不是一个独立运行的服务，不监听端口，也不直接被 Docker Compose 单独启动。

它的作用是为所有 Go 微服务提供统一的基础能力，包括：

```text
配置加载
结构化日志
数据库连接池
链路追踪
HTTP 中间件
事件总线
统一响应格式
```

当前已经接入 shared 的服务包括：

```text
services/gateway
services/auth
```

后续服务也应直接复用 shared，例如：

```text
services/user
services/problem
services/contest
services/submission
services/judge-api
```

---

# 二、目录结构

当前 shared 模块结构如下：

```text
services/shared/

├── config/
│   ├── config.go
│   └── load.go
│
├── database/
│   └── postgres.go
│
├── events/
│   ├── event.go
│   └── nats.go
│
├── logger/
│   └── logger.go
│
├── middleware/
│   ├── logging.go
│   └── recovery.go
│
├── response/
│   └── response.go
│
├── tracing/
│   └── tracing.go
│
├── go.mod
└── go.sum
```

---

# 三、config 配置模块

## 3.1 作用

`config` 模块负责统一加载服务配置。

当前使用：

```text
Viper
```

每个服务都需要在自己的服务目录下提供：

```text
configs/config.yaml
```

例如：

```text
services/gateway/configs/config.yaml
services/auth/configs/config.yaml
```

## 3.2 配置结构

当前统一配置结构包括：

```yaml
service:
  name: gateway-service
  port: 8080

database:
  url: postgres://postgres:password@postgres:5432/ojos?sslmode=disable

jaeger:
  endpoint: ojos-jaeger:4317

nats:
  url: nats://ojos-nats:4222
```

其中：

```text
service.name
```

用于日志服务名、Jaeger service name 等。

```text
service.port
```

用于 go-zero HTTP Server 监听端口。

```text
database.url
```

用于 PostgreSQL 连接池。

```text
jaeger.endpoint
```

用于 OpenTelemetry OTLP gRPC exporter。

```text
nats.url
```

用于 NATS EventBus。

## 3.3 注意事项

在 Docker 容器内部，数据库地址必须写：

```text
postgres:5432
```

不能写：

```text
localhost:5433
```

因为容器内的 `localhost` 指的是当前容器本身，而不是宿主机。

---

# 四、logger 日志模块

## 4.1 作用

`logger` 模块负责创建统一的结构化日志器。

当前使用：

```text
Zap
```

每条日志默认带有：

```text
service
```

字段。

## 4.2 trace 日志注入

`logger.WithTrace(ctx, log)` 会从请求上下文中读取：

```text
trace_id
span_id
```

并写入日志。

示例日志：

```json
{
  "level": "info",
  "msg": "http request",
  "service": "gateway-service",
  "trace_id": "56af8e5d99c1e0f39afcc2f144f63101",
  "span_id": "aa96fb71d55bb95f",
  "method": "GET",
  "path": "/health",
  "status": 200,
  "duration": 0.000309788
}
```

## 4.3 重要说明

`logger.WithTrace` 只负责读取 trace 信息并写入日志。

它不负责：

```text
创建 span
结束 span
导出 span
上报 Jaeger
```

HTTP 请求的 tracing 由 `middleware` 模块中的 OpenTelemetry HTTP instrumentation 负责。

---

# 五、database 数据库模块

## 5.1 作用

`database` 模块负责统一创建 PostgreSQL 连接池。

当前使用：

```text
pgxpool
```

## 5.2 当前能力

`database.NewPostgresPool(ctx, cfg)` 完成：

```text
读取 database.url
解析 pgxpool 配置
设置连接池参数
创建连接池
Ping 检查
返回 *pgxpool.Pool
```

## 5.3 连接池管理

连接池由每个服务的 `App` 持有，并在服务关闭时统一释放。

典型使用方式：

```go
pool, err := database.NewPostgresPool(ctx, cfg)
if err != nil {
    return nil, err
}
```

关闭方式：

```go
pool.Close()
```

---

# 六、tracing 链路追踪模块

## 6.1 作用

`tracing` 模块负责初始化 OpenTelemetry TracerProvider。

当前链路为：

```text
OpenTelemetry SDK
    ↓
OTLP gRPC Exporter
    ↓
Jaeger Collector
    ↓
Jaeger UI
```

## 6.2 Jaeger 配置

Docker Compose 中 Jaeger 需要开启 OTLP：

```yaml
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
```

服务配置中使用：

```yaml
jaeger:
  endpoint: ojos-jaeger:4317
```

## 6.3 当前能力

每个服务启动时调用：

```go
tp, err := tracing.Init(ctx, cfg)
```

初始化成功后，服务会在 Jaeger 中显示为：

```text
gateway-service
auth-service
```

## 6.4 重要经验

开发过程中曾出现：

```text
日志中有 trace_id / span_id
但 Jaeger 中只有 gateway.startup
没有 GET /health
```

最终确认原因是：手写 HTTP span 没有稳定接入 go-zero 的 HTTP 请求链路。

最终解决方式是：

```text
HTTP 请求 tracing 统一交给 otelhttp.NewHandler
并显式传入当前 TracerProvider
```

关键代码：

```go
otelhttp.WithTracerProvider(tp)
```

以及：

```go
otelhttp.WithSpanNameFormatter(func(operation string, r *http.Request) string {
    return r.Method + " " + r.URL.Path
})
```

当前原则：

```text
tracing 包只初始化 TracerProvider
middleware 负责 HTTP trace
logger 只读取 trace_id / span_id
handler 不手写 HTTP span
```

---

# 七、middleware 中间件模块

## 7.1 Recovery Middleware

`Recovery` 中间件用于捕获 handler 中的 panic，避免服务直接崩溃。

作用：

```text
捕获 panic
记录错误日志
返回 500 响应
```

注册方式：

```go
server.Use(func(next http.HandlerFunc) http.HandlerFunc {
    return sharedmw.Recovery(a.Logger, next)
})
```

## 7.2 Logging + HTTP Tracing Middleware

该中间件负责：

```text
记录 HTTP 请求日志
创建 HTTP server span
向 Jaeger 上报请求 trace
将 trace_id / span_id 注入日志
记录 method / path / status / duration
```

当前实现基于：

```text
otelhttp.NewHandler
```

而不是手写：

```go
tracer.Start(...)
```

这样可以保证 HTTP 请求被正确识别为 server span，并在 Jaeger Operation 中显示，例如：

```text
GET /health
```

---

# 八、events 事件模块

## 8.1 作用

`events` 模块封装 NATS EventBus。

当前使用：

```text
NATS
```

## 8.2 事件结构

事件基础字段包括：

```text
id
type
producer
timestamp
payload
```

## 8.3 当前能力

提供：

```go
events.NewBus(cfg)
bus.Publish(ctx, subject, eventType, payload)
bus.Close()
```

## 8.4 当前已验证事件

Gateway health 会发布：

```text
gateway.health.checked
```

Auth health 会发布：

```text
auth.health.checked
```

后续业务事件可以继续扩展：

```text
user.registered
user.login
submission.created
submission.finished
contest.started
contest.ended
```

---

# 九、response 响应模块

## 9.1 作用

`response` 模块负责统一 HTTP JSON 返回格式。

## 9.2 成功响应

```json
{
  "code": 0,
  "msg": "success",
  "data": {}
}
```

## 9.3 错误响应

```json
{
  "code": 10001,
  "msg": "error message"
}
```

## 9.4 使用方式

```go
response.Success(w, data)
response.Error(w, code, msg)
```

## 9.5 示例

Gateway health：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "status": "ok"
  }
}
```

Auth health：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "service": "auth",
    "status": "ok"
  }
}
```

---

# 十、Shared 当前完成状态

当前 shared 已经完成：

```text
config      ✅
logger      ✅
database    ✅
tracing     ✅
events      ✅
middleware  ✅
response    ✅
```

可以支撑后续微服务开发。

---

# 十一、后续可扩展方向

后续 shared 可以继续加入：

```text
JWT 工具
RBAC 中间件
Request ID 中间件
CORS 中间件
统一错误码
统一 validator
Redis client
gRPC client/server helper
Prometheus metrics
```

当前阶段 shared MVP 已经完成，可以稳定复用于 Gateway、Auth 以及后续服务。












对。你现在只写了 `config.rs` 和 `main.rs`，那还差这 3 个文件：

```text
src/event.rs
src/db.rs
src/judge.rs
```

另外还要有：

```text
config/languages.yaml
```

你先按下面补齐。

---

## 1. 确认目录结构

在：

```powershell
cd D:\Untitled-OJ\services\judge-worker
```

应该最终是：

```text
judge-worker
├── Cargo.toml
├── config
│   └── languages.yaml
└── src
    ├── main.rs
    ├── config.rs
    ├── event.rs
    ├── db.rs
    └── judge.rs
```

---

## 2. config/languages.yaml

先建目录：

```powershell
mkdir config
```

新建：

```text
D:\Untitled-OJ\services\judge-worker\config\languages.yaml
```

写：

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

  c11:
    source_file: main.c
    exe_file: main
    compile:
      enabled: true
      command: gcc
      args:
        - "-std=c11"
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

  java17:
    source_file: Main.java
    exe_file: ""
    compile:
      enabled: true
      command: javac
      args:
        - "{source}"
      timeout_ms: 10000
    run:
      command: java
      args:
        - "-cp"
        - "{workdir}"
        - "Main"

  rust:
    source_file: main.rs
    exe_file: main
    compile:
      enabled: true
      command: rustc
      args:
        - "--edition=2021"
        - "-O"
        - "{source}"
        - "-o"
        - "{exe}"
      timeout_ms: 15000
    run:
      command: "{exe}"
      args: []

  go:
    source_file: main.go
    exe_file: main
    compile:
      enabled: true
      command: go
      args:
        - "build"
        - "-o"
        - "{exe}"
        - "{source}"
      timeout_ms: 15000
    run:
      command: "{exe}"
      args: []
```

---

## 3. src/event.rs

新建：

```text
D:\Untitled-OJ\services\judge-worker\src\event.rs
```

写：

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Event {
    pub id: String,

    #[serde(rename = "type")]
    pub event_type: String,

    pub producer: String,
    pub timestamp: String,
    pub payload: serde_json::Value,
}

impl Event {
    pub fn submission_id(&self) -> Option<i64> {
        self.payload
            .get("submission_id")
            .and_then(|v| v.as_i64())
    }
}
```

---

## 4. src/db.rs

新建：

```text
D:\Untitled-OJ\services\judge-worker\src\db.rs
```

写：

```rust
use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

#[derive(Debug)]
pub struct Submission {
    pub id: i64,
    pub problem_id: i64,
    pub user_id: i64,
    pub language: String,
    pub code: String,
}

#[derive(Debug)]
pub struct Problem {
    pub id: i64,
    pub time_limit_ms: i32,
    pub memory_limit_mb: i32,
}

#[derive(Debug)]
pub struct TestCase {
    pub id: i64,
    pub input: String,
    pub output: String,
    pub score: i32,
}

#[derive(Debug)]
pub struct CaseResult {
    pub test_case_id: i64,
    pub status: String,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub message: String,
    pub passed_score: i32,
}

#[derive(Debug)]
pub struct JudgeResult {
    pub status: String,
    pub score: i32,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub message: String,
    pub cases: Vec<CaseResult>,
}

pub async fn load_submission(db: &PgPool, submission_id: i64) -> Result<Submission> {
    let row = sqlx::query(
        r#"
        SELECT id, problem_id, user_id, language, code
        FROM submissions
        WHERE id = $1
        "#,
    )
    .bind(submission_id)
    .fetch_one(db)
    .await
    .context("submission not found")?;

    Ok(Submission {
        id: row.try_get("id")?,
        problem_id: row.try_get("problem_id")?,
        user_id: row.try_get("user_id")?,
        language: row.try_get("language")?,
        code: row.try_get("code")?,
    })
}

pub async fn load_problem(db: &PgPool, problem_id: i64) -> Result<Problem> {
    let row = sqlx::query(
        r#"
        SELECT id, time_limit_ms, memory_limit_mb
        FROM problems
        WHERE id = $1
        "#,
    )
    .bind(problem_id)
    .fetch_one(db)
    .await
    .context("problem not found")?;

    Ok(Problem {
        id: row.try_get("id")?,
        time_limit_ms: row.try_get("time_limit_ms")?,
        memory_limit_mb: row.try_get("memory_limit_mb")?,
    })
}

pub async fn load_test_cases(db: &PgPool, problem_id: i64) -> Result<Vec<TestCase>> {
    let rows = sqlx::query(
        r#"
        SELECT id, input, output, score
        FROM test_cases
        WHERE problem_id = $1
        ORDER BY id
        "#,
    )
    .bind(problem_id)
    .fetch_all(db)
    .await?;

    let mut cases = Vec::with_capacity(rows.len());

    for row in rows {
        cases.push(TestCase {
            id: row.try_get("id")?,
            input: row.try_get("input")?,
            output: row.try_get("output")?,
            score: row.try_get("score")?,
        });
    }

    Ok(cases)
}

pub async fn update_submission_status(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    score: i32,
    time_ms: i32,
    memory_kb: i32,
    message: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE submissions
        SET status = $2,
            score = $3,
            time_ms = $4,
            memory_kb = $5,
            message = $6,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(submission_id)
    .bind(status)
    .bind(score)
    .bind(time_ms)
    .bind(memory_kb)
    .bind(message)
    .execute(db)
    .await?;

    Ok(())
}

pub async fn mark_submission_failed(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    message: &str,
) -> Result<()> {
    update_submission_status(db, submission_id, status, 0, 0, 0, message).await
}

pub async fn save_judge_result(
    db: &PgPool,
    submission_id: i64,
    result: JudgeResult,
) -> Result<()> {
    let mut tx = db.begin().await?;

    sqlx::query(
        r#"
        DELETE FROM submission_cases
        WHERE submission_id = $1
        "#,
    )
    .bind(submission_id)
    .execute(&mut *tx)
    .await?;

    for case in &result.cases {
        sqlx::query(
            r#"
            INSERT INTO submission_cases(
                submission_id,
                test_case_id,
                status,
                time_ms,
                memory_kb,
                message
            )
            VALUES($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(submission_id)
        .bind(case.test_case_id)
        .bind(&case.status)
        .bind(case.time_ms)
        .bind(case.memory_kb)
        .bind(&case.message)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        r#"
        UPDATE submissions
        SET status = $2,
            score = $3,
            time_ms = $4,
            memory_kb = $5,
            message = $6,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(submission_id)
    .bind(&result.status)
    .bind(result.score)
    .bind(result.time_ms)
    .bind(result.memory_kb)
    .bind(&result.message)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}
```

---

## 5. src/judge.rs

新建：

```text
D:\Untitled-OJ\services\judge-worker\src\judge.rs
```

写：

```rust
use anyhow::{Context, Result};
use sqlx::PgPool;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::info;

use crate::config::{render_arg, LanguageConfig, LanguagesConfig};
use crate::db::{
    load_problem, load_submission, load_test_cases, mark_submission_failed, save_judge_result,
    update_submission_status, CaseResult, JudgeResult, Problem, Submission, TestCase,
};

pub async fn handle_submission(
    db: &PgPool,
    languages: Arc<LanguagesConfig>,
    submission_id: i64,
) -> Result<()> {
    info!(submission_id, "start judging");

    update_submission_status(db, submission_id, "RUNNING", 0, 0, 0, "").await?;

    let submission = load_submission(db, submission_id).await?;
    let problem = load_problem(db, submission.problem_id).await?;
    let test_cases = load_test_cases(db, submission.problem_id).await?;

    if test_cases.is_empty() {
        mark_submission_failed(db, submission_id, "SYSTEM_ERROR", "no test cases").await?;
        return Ok(());
    }

    let result = judge_submission(&submission, &problem, &test_cases, &languages).await?;

    save_judge_result(db, submission_id, result).await?;

    info!(submission_id, "judge finished");

    Ok(())
}

async fn judge_submission(
    submission: &Submission,
    problem: &Problem,
    test_cases: &[TestCase],
    languages: &LanguagesConfig,
) -> Result<JudgeResult> {
    let Some(lang) = languages.get(&submission.language) else {
        return Ok(JudgeResult {
            status: "UNSUPPORTED_LANGUAGE".to_string(),
            score: 0,
            time_ms: 0,
            memory_kb: 0,
            message: format!("unsupported language: {}", submission.language),
            cases: vec![],
        });
    };

    let work_dir = TempDir::new().context("create temp dir failed")?;
    let work_path = work_dir.path().to_path_buf();

    let source_path = work_path.join(&lang.source_file);

    let exe_path = if lang.exe_file.is_empty() {
        work_path.join("unused-exe")
    } else {
        work_path.join(&lang.exe_file)
    };

    fs::write(&source_path, &submission.code)
        .await
        .context("write source failed")?;

    if lang.compile.enabled {
        let compile_error = compile(lang, &source_path, &exe_path, &work_path).await?;

        if let Some(message) = compile_error {
            return Ok(JudgeResult {
                status: "COMPILE_ERROR".to_string(),
                score: 0,
                time_ms: 0,
                memory_kb: 0,
                message,
                cases: vec![],
            });
        }
    }

    let mut case_results = Vec::new();
    let mut total_score = 0;
    let mut max_time_ms = 0;
    let mut final_status = "ACCEPTED".to_string();
    let mut final_message = String::new();

    for tc in test_cases {
        let case_result = run_case(lang, &source_path, &exe_path, &work_path, problem, tc).await?;

        if case_result.status == "ACCEPTED" {
            total_score += case_result.passed_score;
        } else if final_status == "ACCEPTED" {
            final_status = case_result.status.clone();
            final_message = case_result.message.clone();
        }

        max_time_ms = max_time_ms.max(case_result.time_ms);
        case_results.push(case_result);

        if final_status != "ACCEPTED" {
            break;
        }
    }

    Ok(JudgeResult {
        status: final_status,
        score: total_score,
        time_ms: max_time_ms,
        memory_kb: 0,
        message: final_message,
        cases: case_results,
    })
}

async fn compile(
    lang: &LanguageConfig,
    source_path: &Path,
    exe_path: &Path,
    work_path: &Path,
) -> Result<Option<String>> {
    let args: Vec<String> = lang
        .compile
        .args
        .iter()
        .map(|arg| render_arg(arg, source_path, exe_path, work_path))
        .collect();

    let mut cmd = Command::new(&lang.compile.command);
    cmd.args(args);
    cmd.current_dir(work_path);

    let compile_future = cmd.output();

    let output = if lang.compile.timeout_ms > 0 {
        match timeout(Duration::from_millis(lang.compile.timeout_ms), compile_future).await {
            Ok(result) => result.context("run compile command failed")?,
            Err(_) => {
                return Ok(Some("compile timeout".to_string()));
            }
        }
    } else {
        compile_future.await.context("run compile command failed")?
    };

    if output.status.success() {
        Ok(None)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(Some(truncate_message(&stderr)))
    }
}

async fn run_case(
    lang: &LanguageConfig,
    source_path: &Path,
    exe_path: &Path,
    work_path: &Path,
    problem: &Problem,
    tc: &TestCase,
) -> Result<CaseResult> {
    let run_command = render_arg(&lang.run.command, source_path, exe_path, work_path);

    let run_args: Vec<String> = lang
        .run
        .args
        .iter()
        .map(|arg| render_arg(arg, source_path, exe_path, work_path))
        .collect();

    let mut child = Command::new(run_command)
        .args(run_args)
        .current_dir(work_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn user program failed")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(tc.input.as_bytes())
            .await
            .context("write stdin failed")?;
    }

    let start = Instant::now();
    let limit = Duration::from_millis(problem.time_limit_ms.max(1) as u64);

    let output = match timeout(limit, child.wait_with_output()).await {
        Ok(result) => result.context("wait user program failed")?,
        Err(_) => {
            return Ok(CaseResult {
                test_case_id: tc.id,
                status: "TIME_LIMIT_EXCEEDED".to_string(),
                time_ms: problem.time_limit_ms,
                memory_kb: 0,
                message: "time limit exceeded".to_string(),
                passed_score: 0,
            });
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as i32;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        return Ok(CaseResult {
            test_case_id: tc.id,
            status: "RUNTIME_ERROR".to_string(),
            time_ms: elapsed_ms,
            memory_kb: 0,
            message: truncate_message(&stderr),
            passed_score: 0,
        });
    }

    let actual = String::from_utf8_lossy(&output.stdout).to_string();

    if normalize_output(&actual) == normalize_output(&tc.output) {
        Ok(CaseResult {
            test_case_id: tc.id,
            status: "ACCEPTED".to_string(),
            time_ms: elapsed_ms,
            memory_kb: 0,
            message: String::new(),
            passed_score: tc.score,
        })
    } else {
        Ok(CaseResult {
            test_case_id: tc.id,
            status: "WRONG_ANSWER".to_string(),
            time_ms: elapsed_ms,
            memory_kb: 0,
            message: format!(
                "expected `{}`, got `{}`",
                truncate_message(&tc.output),
                truncate_message(&actual)
            ),
            passed_score: 0,
        })
    }
}

fn normalize_output(s: &str) -> String {
    s.replace("\r\n", "\n").trim_end().to_string()
}

fn truncate_message(s: &str) -> String {
    const LIMIT: usize = 512;

    let s = s.trim();

    if s.len() <= LIMIT {
        s.to_string()
    } else {
        format!("{}...", &s[..LIMIT])
    }
}
```

---

## 6. 再确认 src/main.rs

你的 `main.rs` 应该是这个结构：

```rust
mod config;
mod db;
mod event;
mod judge;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::config::LanguagesConfig;
use crate::event::Event;
use crate::judge::handle_submission;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().json().init();

    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://ojos-nats:4222".to_string());

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:password@postgres:5432/ojos?sslmode=disable".to_string()
    });

    let languages_path =
        std::env::var("LANGUAGES_CONFIG").unwrap_or_else(|_| "config/languages.yaml".to_string());

    let languages = Arc::new(
        LanguagesConfig::load(&languages_path)
            .await
            .context("load languages config failed")?,
    );

    info!(%nats_url, %database_url, %languages_path, "judge-worker starting");

    let db = PgPool::connect(&database_url)
        .await
        .context("connect postgres failed")?;

    let client = async_nats::connect(nats_url)
        .await
        .context("connect nats failed")?;

    let mut subscriber = client
        .subscribe("submission.created")
        .await
        .context("subscribe submission.created failed")?;

    info!("judge-worker subscribed submission.created");

    while let Some(message) = subscriber.next().await {
        let raw = String::from_utf8_lossy(&message.payload);

        match serde_json::from_slice::<Event>(&message.payload) {
            Ok(event) => {
                let submission_id = event.submission_id();

                info!(
                    event_id = %event.id,
                    event_type = %event.event_type,
                    producer = %event.producer,
                    timestamp = %event.timestamp,
                    submission_id = ?submission_id,
                    "received submission.created"
                );

                if let Some(submission_id) = submission_id {
                    if let Err(err) =
                        handle_submission(&db, languages.clone(), submission_id).await
                    {
                        error!(
                            submission_id,
                            error = %err,
                            "judge submission failed"
                        );

                        let _ = crate::db::mark_submission_failed(
                            &db,
                            submission_id,
                            "SYSTEM_ERROR",
                            &err.to_string(),
                        )
                        .await;
                    }
                } else {
                    warn!(raw = %raw, "submission.created missing submission_id");
                }
            }
            Err(err) => {
                error!(
                    error = %err,
                    raw = %raw,
                    "failed to parse submission.created event"
                );
            }
        }
    }

    Ok(())
}
```

---

## 7. 编译

执行：

```powershell
cd D:\Untitled-OJ\services\judge-worker
cargo fmt
cargo build
```

如果这里报错，把报错贴出来。

编译过了，再改 Dockerfile 和 compose。
