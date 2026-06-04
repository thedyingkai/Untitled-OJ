# Judge API 文档

## 一、模块定位

`judge-api` 是 OJOS Judge 子系统的 HTTP API 层。

它负责处理与“提交”和“评测任务”相关的请求，包括：

```text
创建提交
查询提交摘要
查询提交测试点结果
取消单份提交成绩
重测某题全部提交
```

`judge-api` 不负责：

```text
创建题目
维护题面
维护题解
维护测试数据文件
维护 runner / checker / scorer 配置
执行用户代码
比较用户输出
计算每个 case 结果
```

这些职责分别属于：

```text
problem-api
judge-worker
```

当前职责边界：

```text
problem-api:
    管理题目包与测试数据

judge-api:
    管理提交、任务、cancel、rejudge

judge-worker:
    消费任务、执行判题、写 result.json
```

---

## 二、访问路径

`judge-api` 内部监听：

```text
0.0.0.0:8082
```

内部服务路径：

```text
/judge/*
```

通过 Gateway 暴露：

```text
/api/judge/*
```

正常外部访问必须走 Gateway：

```text
http://localhost:8080/api/judge/*
```

不推荐直接访问：

```text
http://localhost:8082/judge/*
```

原因是：

```text
judge-api 依赖 Gateway 注入可信用户上下文
```

包括：

```text
X-Auth-Verified
X-User-Id
X-Username
X-Roles
```

这些 Header 必须由 Gateway 清理后重新注入，不应由客户端伪造。

---

## 三、当前接口总览

当前 Judge API 接口为：

```http
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
POST /judge/submissions/:id/cancel
POST /judge/problems/:id/rejudge
```

通过 Gateway 访问时：

```http
POST /api/judge/submissions
GET  /api/judge/submissions/:id
GET  /api/judge/submissions/:id/cases
POST /api/judge/submissions/:id/cancel
POST /api/judge/problems/:id/rejudge
```

旧接口已经废弃：

```http
POST /judge/problems
POST /judge/test-cases
```

对应职责已经迁移到：

```text
problem-api
```

---

## 四、认证与授权

### 4.1 认证

所有 Judge API 正常业务接口都应通过 Gateway 访问，并携带：

```http
Authorization: Bearer <token>
```

Gateway 负责：

```text
解析 JWT
校验 token
清理客户端伪造的用户 Header
注入可信用户上下文 Header
代理请求到 judge-api
```

`judge-api` 从上下文中读取当前用户，不从请求体读取：

```text
user_id
```

请求体中的 `user_id` 不可信，也不应该存在。

---

### 4.2 权限点

当前 Judge API 已接入 Permission Core。

当前权限检查点：

```text
POST /judge/submissions
    -> judge.submit @ system:0

POST /judge/submissions/:id/cancel
    -> problem.manage.data @ problem:{problem_id}

POST /judge/problems/:id/rejudge
    -> problem.manage.data @ problem:{id}
```

后续建议补充：

```text
GET /judge/submissions/:id
    -> submission.view.own / submission.view.all

GET /judge/submissions/:id/cases
    -> submission.view.own / submission.view.all
       + contest feedback policy
```

比赛环境下，查询接口还需要结合：

```text
比赛是否封榜
赛制反馈策略
是否为本人提交
是否为管理员
题目是否可见
```

---

## 五、提交状态

当前 Judge API 可能返回的提交状态包括：

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

状态含义：

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

主要状态流转：

```text
PENDING
  ↓
JUDGING
  ↓
ACCEPTED / WRONG_ANSWER / COMPILE_ERROR / RUNTIME_ERROR
/ TIME_LIMIT_EXCEEDED / SYSTEM_ERROR / UNSUPPORTED_LANGUAGE
```

Cancel 流转：

```text
任意已存在提交
  ↓
CANCELLED
```

Rejudge 流转：

```text
该题全部提交，包括 CANCELLED
  ↓
PENDING
  ↓
重新评测
```

---

## 六、POST /judge/submissions

### 6.1 功能

创建一份新的提交。

该接口负责：

```text
读取当前登录用户
检查 judge.submit 权限
检查 problem_id
检查 language
检查 code
读取 problem.package_dir
创建 submissions 记录
将源码写入 storage/submissions/{id}/source/
创建初始 result.json
向 Redis Stream 投递判题任务
返回 submission_id
```

该接口不直接执行判题。

判题由：

```text
judge-worker
```

异步完成。

---

### 6.2 路径

内部路径：

```http
POST /judge/submissions
```

Gateway 路径：

```http
POST /api/judge/submissions
```

---

### 6.3 请求体

```json
{
  "problem_id": 2,
  "language": "cpp17",
  "code": "#include <bits/stdc++.h>\nusing namespace std;\nint main(){long long a,b;cin>>a>>b;cout<<a+b<<'\\n';}"
}
```

字段说明：

| 字段           | 类型      | 必填 | 说明            |
| ------------ | ------- | -: | ------------- |
| `problem_id` | integer |  是 | 题目 ID         |
| `language`   | string  |  是 | 语言，例如 `cpp17` |
| `code`       | string  |  是 | 用户源码          |

约束：

```text
problem_id 必须大于 0
language 不能为空
code 不能为空
```

当前不支持请求体传入：

```text
user_id
```

用户身份必须来自 Gateway 注入的上下文。

---

### 6.4 响应

响应示例：

```json
{
  "submission_id": 20,
  "status": "PENDING",
  "code_path": "/data/ojos/submissions/20/source/main.cpp",
  "result_path": "/data/ojos/submissions/20/result.json"
}
```

字段说明：

| 字段              | 说明                 |
| --------------- | ------------------ |
| `submission_id` | 新提交 ID             |
| `status`        | 初始状态，通常为 `PENDING` |
| `code_path`     | 容器内源码路径            |
| `result_path`   | 容器内结果文件路径          |

---

### 6.5 副作用

成功创建提交后，会产生：

```text
数据库 submissions 记录
storage/submissions/{id}/source/main.cpp
storage/submissions/{id}/result.json
Redis Stream 消息
```

Redis Stream：

```text
ojos:judge:submissions
```

消息字段：

```text
type          submission.created
producer      judge-api-service
submission_id <id>
created_at    <UTC timestamp>
```

---

### 6.6 PowerShell 示例

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

---

## 七、GET /judge/submissions/:id

### 7.1 功能

查询提交摘要。

该接口返回数据库 `submissions` 表中的摘要字段，不返回完整源码，也不直接返回所有 case 详情。

完整 case 详情应通过：

```http
GET /judge/submissions/:id/cases
```

查询。

---

### 7.2 路径

内部路径：

```http
GET /judge/submissions/:id
```

Gateway 路径：

```http
GET /api/judge/submissions/:id
```

---

### 7.3 路径参数

| 参数   | 类型      | 说明            |
| ---- | ------- | ------------- |
| `id` | integer | submission ID |

---

### 7.4 响应

响应示例：

```json
{
  "id": 20,
  "problem_id": 2,
  "user_id": 2,
  "language": "cpp17",
  "status": "ACCEPTED",
  "score": 100,
  "time_ms": 21,
  "memory_kb": 0,
  "message": "",
  "code_path": "/data/ojos/submissions/20/source/main.cpp",
  "code_sha256": "f1bd87c840d456fa899a0ad75b17ea0b84fefb5dc554c63db6f535ca0f2b0d82",
  "result_path": "/data/ojos/submissions/20/result.json",
  "judged_at": "2026-06-04T13:28:04.624893Z",
  "cancelled_at": "",
  "cancel_reason": ""
}
```

字段说明：

| 字段              | 说明                           |
| --------------- | ---------------------------- |
| `id`            | 提交 ID                        |
| `problem_id`    | 题目 ID                        |
| `user_id`       | 提交用户 ID                      |
| `language`      | 提交语言                         |
| `status`        | 当前状态                         |
| `score`         | 总分                           |
| `time_ms`       | 总耗时或最大 case 耗时，按 worker 当前实现 |
| `memory_kb`     | 当前暂为 0，后续接入 cgroup v2        |
| `message`       | 错误信息或摘要信息                    |
| `code_path`     | 源码路径                         |
| `code_sha256`   | 源码 SHA-256                   |
| `result_path`   | 完整结果 JSON 路径                 |
| `judged_at`     | 最近评测完成时间                     |
| `cancelled_at`  | 取消时间                         |
| `cancel_reason` | 取消原因                         |

---

### 7.5 PowerShell 示例

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/20" `
  -Headers $headers
```

---

## 八、GET /judge/submissions/:id/cases

### 8.1 功能

查询提交的测试点结果。

当前该接口从：

```text
submissions.result_path
```

指向的：

```text
result.json
```

读取 `cases` 数组。

不再从数据库表：

```text
submission_cases
```

读取。

---

### 8.2 路径

内部路径：

```http
GET /judge/submissions/:id/cases
```

Gateway 路径：

```http
GET /api/judge/submissions/:id/cases
```

---

### 8.3 路径参数

| 参数   | 类型      | 说明            |
| ---- | ------- | ------------- |
| `id` | integer | submission ID |

---

### 8.4 响应

响应示例：

```json
{
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

字段说明：

| 字段                 | 说明               |
| ------------------ | ---------------- |
| `case_no`          | 测试点编号            |
| `status`           | 测试点状态            |
| `score`            | 测试点得分            |
| `time_ms`          | 测试点耗时            |
| `memory_kb`        | 当前暂为 0           |
| `stdout_path`      | 用户程序 stdout 文件路径 |
| `stderr_path`      | 用户程序 stderr 文件路径 |
| `checker_log_path` | checker 日志路径     |
| `message`          | 测试点错误信息          |

---

### 8.5 PowerShell 示例

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/20/cases" `
  -Headers $headers
```

---

### 8.6 反馈策略说明

当前开发阶段直接返回完整 case 信息。

比赛环境下，该接口后续必须接入反馈策略，例如：

```text
ACM 赛时可能不返回详细 case
OI 赛时可能返回分数但隐藏数据点详情
IOI 可能按 subtask / feedback policy 返回部分信息
封榜后可能限制部分字段
管理员可以查看完整结果
```

---

## 九、POST /judge/submissions/:id/cancel

### 9.1 功能

取消单份提交的成绩。

Cancel 的语义是：

```text
取消这份提交当前成绩
```

不是删除提交，也不是删除源码和 result.json。

取消后：

```text
submissions.status = CANCELLED
submissions.cancelled_at = 当前时间
submissions.cancelled_by = 当前用户
submissions.cancel_reason = 请求原因
```

---

### 9.2 路径

内部路径：

```http
POST /judge/submissions/:id/cancel
```

Gateway 路径：

```http
POST /api/judge/submissions/:id/cancel
```

---

### 9.3 权限

需要：

```text
problem.manage.data @ problem:{problem_id}
```

其中 `problem_id` 来自该 submission 所属题目。

也就是说，能取消某份提交成绩的人，必须拥有该提交所属题目的数据管理权限。

---

### 9.4 路径参数

| 参数   | 类型      | 说明            |
| ---- | ------- | ------------- |
| `id` | integer | submission ID |

---

### 9.5 请求体

```json
{
  "reason": "manual cancel test"
}
```

字段说明：

| 字段       | 类型     | 必填 | 说明   |
| -------- | ------ | -: | ---- |
| `reason` | string |  否 | 取消原因 |

---

### 9.6 响应

响应示例：

```json
{
  "submission_id": 28,
  "status": "CANCELLED"
}
```

---

### 9.7 PowerShell 示例

```powershell
$cancelObj = @{
  reason = "test cancel"
}

$json = $cancelObj | ConvertTo-Json -Compress
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)

Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/submissions/28/cancel" `
  -ContentType "application/json; charset=utf-8" `
  -Headers $headers `
  -Body $bytes
```

---

## 十、POST /judge/problems/:id/rejudge

### 10.1 功能

重测某题的全部提交。

Rejudge 的语义是：

```text
重测该题所有 submission
```

包括：

```text
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
CANCELLED
```

因此，`CANCELLED` 提交在 rejudge 后也会重新进入评测流程。

---

### 10.2 路径

内部路径：

```http
POST /judge/problems/:id/rejudge
```

Gateway 路径：

```http
POST /api/judge/problems/:id/rejudge
```

---

### 10.3 权限

需要：

```text
problem.manage.data @ problem:{id}
```

---

### 10.4 路径参数

| 参数   | 类型      | 说明         |
| ---- | ------- | ---------- |
| `id` | integer | problem ID |

---

### 10.5 请求体

当前不需要请求体。

---

### 10.6 响应

响应示例：

```json
{
  "problem_id": 2,
  "enqueued": 10
}
```

字段说明：

| 字段           | 说明                  |
| ------------ | ------------------- |
| `problem_id` | 被重测的题目 ID           |
| `enqueued`   | 重新投递的 submission 数量 |

---

### 10.7 副作用

执行 rejudge 后，会对该题所有提交执行：

```text
status = PENDING
score = 0
time_ms = 0
memory_kb = 0
message = ""
judged_at = NULL
cancelled_at = NULL
cancelled_by = NULL
cancel_reason = ""
```

并向 Redis Stream 投递每个 submission 的判题任务。

---

### 10.8 PowerShell 示例

```powershell
Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/problems/2/rejudge" `
  -Headers $headers
```

---

## 十一、错误响应

当前部分接口可能仍返回 go-zero 默认错误或简单错误文本。

后续应统一为 JSON：

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

当前常见错误：

| 场景             | 可能错误                           |
| -------------- | ------------------------------ |
| 未登录            | `unauthorized`                 |
| token 无效       | `invalid authorization header` |
| 权限不足           | `forbidden`                    |
| 题目不存在          | `problem not found`            |
| 提交不存在          | `submission not found`         |
| 语言不支持          | `UNSUPPORTED_LANGUAGE`         |
| 题目包损坏          | `SYSTEM_ERROR`                 |
| result.json 缺失 | `SYSTEM_ERROR` 或 cases 查询失败    |

---

## 十二、常用测试函数

开发阶段可在 PowerShell 中定义：

```powershell
function Submit-Code {
  param(
    [string]$Code,
    [string]$Language = "cpp17",
    [int]$ProblemId = 2
  )

  $submitObj = @{
    problem_id = $ProblemId
    language = $Language
    code = $Code
  }

  $json = $submitObj | ConvertTo-Json -Compress
  $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)

  Invoke-RestMethod `
    -Method Post `
    -Uri "http://localhost:8080/api/judge/submissions" `
    -ContentType "application/json; charset=utf-8" `
    -Headers $headers `
    -Body $bytes
}

function Show-Submission {
  param([int]$Id)

  Invoke-RestMethod `
    -Method Get `
    -Uri "http://localhost:8080/api/judge/submissions/$Id" `
    -Headers $headers
}

function Wait-Submission {
  param(
    [int]$Id,
    [int]$Seconds = 10
  )

  for ($i = 0; $i -lt $Seconds; $i++) {
    $s = Show-Submission $Id
    if ($s.status -ne "PENDING" -and $s.status -ne "JUDGING") {
      return $s
    }
    Start-Sleep -Seconds 1
  }

  return Show-Submission $Id
}
```

---

## 十三、验收用例

当前 Judge API 应至少通过以下验收：

```text
AC
WA
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
末尾空格和末尾空行忽略
行内空格不同判 WRONG_ANSWER
用户程序无法读取题目答案文件
cancel 单份提交
rejudge 重测包括 CANCELLED
查询 /submissions/:id/cases
```

详细验收记录见：

```text
docs/judge/validation.md
```

---

## 十四、接口边界总结

当前 Judge API 的边界是：

```text
管理 submission
管理 judge task
管理 cancel
管理 rejudge
读取 result.json
```

不是：

```text
Problem API
Dataset API
Runner
Checker
Scorer
Contest Feedback Policy
```

这条边界必须保持清晰。

后续新增交互题、通信题、提交答案题、子任务、捆绑点时，优先扩展：

```text
runner
checker
scorer
problem package
result.json
```

而不是把所有逻辑堆进 `judge-api`。
