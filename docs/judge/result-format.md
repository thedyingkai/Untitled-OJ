# Judge Result Format 文档

## 一、模块定位

`result.json` 是 OJOS Judge 子系统中保存完整评测结果的结构化文件。

当前 OJOS 的设计原则是：

```text
数据库保存摘要
result.json 保存完整结构化结果
case 目录保存原始运行产物
```

也就是说：

```text
submissions 表：
    status
    score
    time_ms
    memory_kb
    message
    code_path
    result_path

result.json：
    submission_id
    status
    score
    time_ms
    memory_kb
    message
    cases[]

cases/{case_no}/：
    stdin.txt
    stdout.txt
    stderr.txt
    checker.log
```

`result.json` 是：

```text
GET /judge/submissions/:id/cases
```

的数据来源。

当前不再使用：

```text
submission_cases
```

保存测试点结果。

---

## 二、文件位置

每份提交都有一个 `result.json`。

宿主机路径：

```text
storage/submissions/{submission_id}/result.json
```

例如：

```text
storage/submissions/20/result.json
```

容器内路径：

```text
/data/ojos/submissions/{submission_id}/result.json
```

例如：

```text
/data/ojos/submissions/20/result.json
```

数据库字段：

```text
submissions.result_path
```

保存容器内路径。

---

## 三、整体结构

当前 `result.json` 的基础结构：

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

顶层字段：

| 字段              | 类型      | 说明          |
| --------------- | ------- | ----------- |
| `submission_id` | integer | 提交 ID       |
| `status`        | string  | 总评测状态       |
| `score`         | integer | 总分          |
| `time_ms`       | integer | 总耗时或汇总耗时    |
| `memory_kb`     | integer | 内存峰值，当前暂为 0 |
| `message`       | string  | 总错误信息或摘要    |
| `cases`         | array   | 测试点结果数组     |

---

## 四、Case 结构

`cases` 数组中的每个元素表示一个测试点结果。

结构：

```json
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
```

字段说明：

| 字段                 | 类型      | 说明               |
| ------------------ | ------- | ---------------- |
| `case_no`          | integer | 测试点编号            |
| `status`           | string  | 测试点状态            |
| `score`            | integer | 测试点得分            |
| `time_ms`          | integer | 测试点耗时            |
| `memory_kb`        | integer | 测试点内存峰值，当前暂为 0   |
| `stdout_path`      | string  | 用户程序 stdout 文件路径 |
| `stderr_path`      | string  | 用户程序 stderr 文件路径 |
| `checker_log_path` | string  | checker 日志文件路径   |
| `message`          | string  | 测试点错误信息          |

---

## 五、状态枚举

当前支持的总状态和 case 状态包括：

```text
PENDING
JUDGING
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
CANCELLED
```

含义：

| 状态                     | 含义     |
| ---------------------- | ------ |
| `PENDING`              | 等待评测   |
| `JUDGING`              | 正在评测   |
| `ACCEPTED`             | 通过     |
| `WRONG_ANSWER`         | 答案错误   |
| `COMPILE_ERROR`        | 编译错误   |
| `RUNTIME_ERROR`        | 运行时错误  |
| `TIME_LIMIT_EXCEEDED`  | 超时     |
| `SYSTEM_ERROR`         | 系统错误   |
| `UNSUPPORTED_LANGUAGE` | 不支持的语言 |
| `CANCELLED`            | 成绩已取消  |

注意：

```text
COMPILE_ERROR 通常只出现在顶层 status
```

因为编译失败时还没有进入测试点运行阶段，`cases` 一般为空。

---

## 六、PENDING 初始结果

提交刚创建时，`judge-api` 可以写入初始 `result.json`。

示例：

```json
{
  "submission_id": 20,
  "status": "PENDING",
  "score": 0,
  "time_ms": 0,
  "memory_kb": 0,
  "message": "",
  "cases": []
}
```

此时：

```text
submissions.status = PENDING
submissions.score = 0
submissions.time_ms = 0
submissions.memory_kb = 0
```

`cases` 为空是正常的，因为 worker 尚未开始评测。

---

## 七、JUDGING 中间状态

当前通常不强制持续更新 `result.json` 为 `JUDGING`，但允许未来扩展。

可选格式：

```json
{
  "submission_id": 20,
  "status": "JUDGING",
  "score": 0,
  "time_ms": 0,
  "memory_kb": 0,
  "message": "",
  "cases": []
}
```

数据库中会在 worker claim 成功后更新：

```text
submissions.status = JUDGING
```

是否同步更新 `result.json`，由 worker 实现决定。

当前开发阶段可只在最终结果时覆盖 `result.json`。

---

## 八、ACCEPTED 示例

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

对应文件：

```text
stdout.txt      = 3
stderr.txt      = 空
checker.log     = accepted
```

---

## 九、WRONG_ANSWER 示例

```json
{
  "submission_id": 21,
  "status": "WRONG_ANSWER",
  "score": 0,
  "time_ms": 16,
  "memory_kb": 0,
  "message": "wrong answer",
  "cases": [
    {
      "case_no": 1,
      "status": "WRONG_ANSWER",
      "score": 0,
      "time_ms": 16,
      "memory_kb": 0,
      "stdout_path": "/data/ojos/submissions/21/cases/001/stdout.txt",
      "stderr_path": "/data/ojos/submissions/21/cases/001/stderr.txt",
      "checker_log_path": "/data/ojos/submissions/21/cases/001/checker.log",
      "message": "wrong answer"
    }
  ]
}
```

对应 `checker.log` 示例：

```text
expected: 3
actual: 0
```

---

## 十、COMPILE_ERROR 示例

编译错误时，`cases` 通常为空。

```json
{
  "submission_id": 22,
  "status": "COMPILE_ERROR",
  "score": 0,
  "time_ms": 0,
  "memory_kb": 0,
  "message": "sandbox_status: RuntimeError\nsandbox_message: process exited with code 1\n\n[stdout]\n\n[stderr]\n/work/main.cpp: In function 'int main()':\n/work/main.cpp:6:22: error: expected ';' before 'return'",
  "cases": []
}
```

对应文件：

```text
build/compile.log
build/compile.stdout.log
build/compile.stderr.log
```

编译错误信息应从：

```text
compile.log
```

读取。

---

## 十一、RUNTIME_ERROR 示例

```json
{
  "submission_id": 23,
  "status": "RUNTIME_ERROR",
  "score": 0,
  "time_ms": 12,
  "memory_kb": 0,
  "message": "process exited with code 1",
  "cases": [
    {
      "case_no": 1,
      "status": "RUNTIME_ERROR",
      "score": 0,
      "time_ms": 12,
      "memory_kb": 0,
      "stdout_path": "/data/ojos/submissions/23/cases/001/stdout.txt",
      "stderr_path": "/data/ojos/submissions/23/cases/001/stderr.txt",
      "checker_log_path": "/data/ojos/submissions/23/cases/001/checker.log",
      "message": "process exited with code 1"
    }
  ]
}
```

对应 `checker.log`：

```text
process exited with code 1
```

注意：

```text
RUNTIME_ERROR message 不应混入大量 nsjail [I] Mount 日志
```

应只保留：

```text
process exited with code ...
```

以及必要的用户 stderr 摘要。

---

## 十二、TIME_LIMIT_EXCEEDED 示例

```json
{
  "submission_id": 24,
  "status": "TIME_LIMIT_EXCEEDED",
  "score": 0,
  "time_ms": 1000,
  "memory_kb": 0,
  "message": "time limit exceeded",
  "cases": [
    {
      "case_no": 1,
      "status": "TIME_LIMIT_EXCEEDED",
      "score": 0,
      "time_ms": 1000,
      "memory_kb": 0,
      "stdout_path": "/data/ojos/submissions/24/cases/001/stdout.txt",
      "stderr_path": "/data/ojos/submissions/24/cases/001/stderr.txt",
      "checker_log_path": "/data/ojos/submissions/24/cases/001/checker.log",
      "message": "time limit exceeded"
    }
  ]
}
```

对应 `checker.log`：

```text
time limit exceeded
```

---

## 十三、SYSTEM_ERROR 示例

系统错误表示 Judge Worker、题目包、文件系统或运行环境异常，而不是用户代码本身错误。

例如：

```text
load problem.yaml failed
load cases.yaml failed
source file missing
answer file missing
nsjail launch failed
result.json write failed
```

示例：

```json
{
  "submission_id": 25,
  "status": "SYSTEM_ERROR",
  "score": 0,
  "time_ms": 0,
  "memory_kb": 0,
  "message": "load cases.yaml failed: /data/ojos/problems/2-a-plus-b/tests/cases.yaml",
  "cases": []
}
```

`SYSTEM_ERROR` 应用于：

```text
系统或题目配置错误
```

不应该用于：

```text
用户代码编译错误
用户代码运行错误
用户代码输出错误
```

---

## 十四、UNSUPPORTED_LANGUAGE 示例

当提交语言未在 `languages.yaml` 中配置时，应返回：

```json
{
  "submission_id": 26,
  "status": "UNSUPPORTED_LANGUAGE",
  "score": 0,
  "time_ms": 0,
  "memory_kb": 0,
  "message": "unsupported language: pascal",
  "cases": []
}
```

---

## 十五、CANCELLED 示例

Cancel 通常由 `judge-api` 更新数据库状态。

可选地也可以同步更新 `result.json`：

```json
{
  "submission_id": 27,
  "status": "CANCELLED",
  "score": 0,
  "time_ms": 0,
  "memory_kb": 0,
  "message": "manual cancel test",
  "cases": [
    {
      "case_no": 1,
      "status": "ACCEPTED",
      "score": 100,
      "time_ms": 21,
      "memory_kb": 0,
      "stdout_path": "/data/ojos/submissions/27/cases/001/stdout.txt",
      "stderr_path": "/data/ojos/submissions/27/cases/001/stderr.txt",
      "checker_log_path": "/data/ojos/submissions/27/cases/001/checker.log",
      "message": ""
    }
  ]
}
```

当前也允许：

```text
数据库 status = CANCELLED
result.json 仍保留上一次评测结果
```

因为 cancel 的事实源是数据库中的：

```text
submissions.status
submissions.cancelled_at
submissions.cancelled_by
submissions.cancel_reason
```

后续是否同步改写 `result.json`，需要统一策略。

---

## 十六、路径字段规范

当前 `stdout_path`、`stderr_path`、`checker_log_path` 使用容器内路径。

示例：

```text
/data/ojos/submissions/20/cases/001/stdout.txt
```

不使用宿主机路径：

```text
D:\Untitled-OJ\storage\submissions\20\cases\001\stdout.txt
```

原因：

```text
服务运行在容器内
数据库和 result.json 面向服务内部路径
```

宿主机调试时手动转换：

```text
/data/ojos/submissions/20
    ↓
D:\Untitled-OJ\storage\submissions\20
```

后续如果引入对象存储，应考虑改为逻辑 URI：

```text
storage://submissions/20/cases/001/stdout.txt
```

或：

```text
s3://bucket/submissions/20/cases/001/stdout.txt
```

当前先保留容器内路径。

---

## 十七、数据库摘要同步规则

每次 worker 写入最终 `result.json` 后，应同步更新 `submissions` 表摘要字段。

映射关系：

| result.json 字段 | submissions 字段 |
| -------------- | -------------- |
| `status`       | `status`       |
| `score`        | `score`        |
| `time_ms`      | `time_ms`      |
| `memory_kb`    | `memory_kb`    |
| `message`      | `message`      |
| 写入时间           | `judged_at`    |
| 更新时间           | `updated_at`   |

也就是说：

```text
数据库用于快速查询
result.json 用于完整结果
```

二者应保持一致。

如果不一致，以数据库状态作为列表查询入口，以 `result.json` 作为详情调试依据。

---

## 十八、Case 汇总规则

当前默认 scorer 为：

```text
default-sum-scorer
```

基础规则：

```text
case.status == ACCEPTED:
    case.score = cases.yaml 中定义的 score

case.status != ACCEPTED:
    case.score = 0
```

总分：

```text
result.score = sum(case.score)
```

总状态建议优先级：

```text
COMPILE_ERROR
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
TIME_LIMIT_EXCEEDED
RUNTIME_ERROR
WRONG_ANSWER
ACCEPTED
```

解释：

```text
编译错误没有 case，直接 COMPILE_ERROR
系统错误优先于普通错误
TLE / RE 属于运行失败
WA 属于正常运行但答案错误
全部 case ACCEPTED 才是 ACCEPTED
```

当前传统题可按此规则。

后续子任务 / 捆绑点 / IOI / ACM 需要扩展 scorer。

---

## 十九、memory_kb 当前规则

当前：

```text
memory_kb = 0
```

是已知限制。

原因：

```text
当前只做了资源限制，没有接入 cgroup v2 峰值内存统计
```

当前不要伪造：

```text
memory_kb
```

后续应由 Runner / Sandbox 层返回真实内存峰值：

```text
case.memory_kb
submission.memory_kb
```

可能来源：

```text
cgroup v2 memory.peak
rusage
nsjail report
```

最终以 cgroup v2 为主。

---

## 二十、message 截断规则

`message` 用于摘要展示，不应写入无限长内容。

建议限制：

```text
512 字符
```

长日志应保存在文件中，例如：

```text
compile.log
stderr.txt
checker.log
```

`message` 只保存摘要。

推荐规则：

```text
COMPILE_ERROR:
    message = compile.log 摘要

RUNTIME_ERROR:
    message = exit code + 用户 stderr 摘要

WRONG_ANSWER:
    message = wrong answer

TIME_LIMIT_EXCEEDED:
    message = time limit exceeded

SYSTEM_ERROR:
    message = 系统错误摘要
```

---

## 二十一、result.json 与 cases API

当前接口：

```http
GET /judge/submissions/:id/cases
```

读取流程：

```text
读取 submissions.result_path
读取 result.json
解析 cases 数组
返回给客户端
```

因此：

```text
cases API 不依赖 submission_cases 表
```

如果 `result.json` 缺失或损坏，应返回错误。

开发阶段可返回：

```text
SYSTEM_ERROR 或 internal error
```

后续应统一错误响应。

---

## 二十二、反馈策略

当前开发阶段，`result.json` 包含完整 case 结果。

但比赛环境下，不一定全部展示给普通用户。

例如：

```text
ACM:
    赛时通常只显示总状态
    不显示详细 case

OI:
    可能显示分数
    可能隐藏具体数据点

IOI:
    可能按 subtask feedback policy 显示

封榜:
    可能限制提交结果可见性

管理员:
    可以查看完整 result.json
```

因此：

```text
result.json 是完整事实
API 返回内容要受 contest feedback policy 过滤
```

当前 `GET /judge/submissions/:id/cases` 还没有接入反馈策略。

---

## 二十三、后续扩展方向

当前 `result.json` 是传统题基础格式。

后续需要支持：

```text
subtasks
groups
bundles
special judge
interactive logs
communication logs
output-only files
heuristic score details
compiler resource usage
runner metadata
sandbox metadata
```

可能扩展格式：

```json
{
  "submission_id": 100,
  "status": "PARTIAL_ACCEPTED",
  "score": 70,
  "time_ms": 120,
  "memory_kb": 32768,
  "message": "",
  "groups": [
    {
      "group_id": 1,
      "score": 30,
      "status": "ACCEPTED",
      "cases": [1, 2, 3]
    },
    {
      "group_id": 2,
      "score": 40,
      "status": "WRONG_ANSWER",
      "cases": [4, 5]
    }
  ],
  "cases": []
}
```

当前不急着实现，但设计时要避免把格式写死到只能支持传统题。

---

## 二十四、兼容性策略

当前处于开发阶段。

原则：

```text
不兼容旧的 no: 0 测试点格式
不兼容旧的 test_cases 数据库格式
不兼容旧的 submission_cases 查询格式
不维护 submission version
rejudge 直接覆盖旧 result.json
```

原因：

```text
项目仍在开发阶段
旧格式没有维护价值
越早清理越少技术债
```

未来进入稳定版本后，再考虑：

```text
result schema version
migration tool
backward compatibility
```

当前可以在 `result.json` 中暂时不加 schema 字段。

后续建议加入：

```json
{
  "schema": "ojos.judge.result.v1",
  "submission_id": 20
}
```

但现在不是必须。

---

## 二十五、当前结论

当前 `result.json` 是 OJOS Judge 结果系统的核心文件。

它承担：

```text
完整结构化评测结果
测试点结果列表
原始产物路径索引
cases API 数据来源
后续 scorer / runner / checker 扩展基础
```

当前核心原则是：

```text
数据库保存摘要
result.json 保存结构化完整结果
case 目录保存原始运行产物
```

后续扩展 OI / IOI / 子任务 / 捆绑点 / SPJ / 交互题时，应优先扩展：

```text
result.json
```

而不是恢复：

```text
submission_cases
```

这种数据库大表结果存储方式。
