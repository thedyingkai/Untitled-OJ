# Judge Validation 文档

## 一、文档目的

本文档记录 OJOS Judge 子系统当前阶段的功能验收结果。

当前验收目标是确认：

```text
judge-api 可以正确创建提交
judge-api 可以正确查询提交
judge-api 可以正确 cancel
judge-api 可以正确 rejudge
judge-worker 可以消费 Redis Stream
judge-worker 可以读取题目包
judge-worker 可以使用 nsjail 编译运行
checker 可以正确判定输出
result.json 可以正确落盘
/submissions/:id/cases 可以正确读取 result.json
```

当前验证对象：

```text
services/judge-api
services/judge-worker
storage/problems
storage/submissions
Redis Streams
PostgreSQL submissions
nsjail sandbox
default-trim-checker
default-sum-scorer
```

当前验证题目：

```text
problem_id = 2
slug = 2-a-plus-b
title = A+B Problem
```

当前题目包路径：

```text
storage/problems/2-a-plus-b/
```

当前提交结果路径：

```text
storage/submissions/{submission_id}/
```

---

## 二、当前已验证结论

当前 Judge 主链路已经通过验收。

已验证功能：

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
Redis Stream 消息可以被 worker 消费并 XACK
```

当前可以确认：

```text
traditional-runner 可用
default-trim-checker 可用
default-sum-scorer 可用
nsjail 基础隔离可用
submission 文件化存储可用
result.json 结果格式可用
```

当前仍未完成：

```text
memory_kb 真实统计
多语言完整验收
输出大小限制
SPJ
子任务
捆绑点
交互题
通信题
提交答案题
```

---

## 三、基础服务验收

### 3.1 Docker 服务状态

检查命令：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose ps
```

预期核心服务：

```text
ojos-gateway        Up
ojos-auth           Up
ojos-problem-api    Up
ojos-judge-api      Up
ojos-judge-worker   Up
ojos-postgres       Up / Healthy
ojos-redis          Up
ojos-jaeger         Up
```

不应存在：

```text
ojos-nats
```

因为 Judge Queue 已经从 NATS 迁移到 Redis Streams。

---

### 3.2 Judge Worker 日志

检查命令：

```powershell
docker logs ojos-judge-worker --tail 100
```

正常启动日志应包含：

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

说明：

```text
Consumer Group already exists 不是错误
```

---

### 3.3 Redis Stream 状态

检查 Stream：

```powershell
docker exec -it ojos-redis redis-cli XINFO STREAM ojos:judge:submissions
```

检查 Consumer Group：

```powershell
docker exec -it ojos-redis redis-cli XINFO GROUPS ojos:judge:submissions
```

检查 Pending：

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

正常情况下，任务处理完成后：

```text
XPENDING = 0
```

说明 worker 已经 XACK 消息。

---

## 四、测试准备

### 4.1 登录并准备 Headers

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

如果返回：

```text
invalid authorization header
```

应重新登录并刷新 `$token` / `$headers`。

---

### 4.2 定义测试辅助函数

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

## 五、AC 验收

### 5.1 测试代码

```powershell
$acCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    long long a, b;
    cin >> a >> b;
    cout << a + b << '\n';
    return 0;
}
'@

$ac = Submit-Code $acCode
$ac
Wait-Submission $ac.submission_id
```

### 5.2 预期结果

```text
status = ACCEPTED
score = 100
message = ""
```

### 5.3 文件检查

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($ac.submission_id)\result.json" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($ac.submission_id)\cases\001\stdout.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($ac.submission_id)\cases\001\checker.log" -Encoding UTF8
```

预期：

```text
stdout.txt = 3
checker.log = accepted
```

验收结论：

```text
AC 通过
```

---

## 六、WA 验收

### 6.1 测试代码

```powershell
$waCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    cout << 0 << '\n';
    return 0;
}
'@

$wa = Submit-Code $waCode
$wa
Wait-Submission $wa.submission_id
```

### 6.2 预期结果

```text
status = WRONG_ANSWER
score = 0
message = wrong answer
```

### 6.3 文件检查

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($wa.submission_id)\result.json" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($wa.submission_id)\cases\001\checker.log" -Encoding UTF8
```

预期 checker 日志类似：

```text
expected: 3
actual: 0
```

验收结论：

```text
WA 通过
```

---

## 七、末尾空格 / 末尾空行验收

### 7.1 测试代码

```powershell
$trimAcCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    cout << "3   \n\n\n";
    return 0;
}
'@

$trimAc = Submit-Code $trimAcCode
$trimAc
Wait-Submission $trimAc.submission_id
```

### 7.2 预期结果

```text
status = ACCEPTED
score = 100
```

### 7.3 说明

当前 default-trim-checker 规则：

```text
忽略每行末尾空格和 Tab
忽略末尾空行
不忽略行内空格
```

因此：

```text
actual:   "3   \n\n\n"
expected: "3\n"
```

应判定为：

```text
ACCEPTED
```

验收结论：

```text
末尾空格 / 末尾空行忽略通过
```

---

## 八、行内空格 WA 验收

### 8.1 测试代码

```powershell
$innerSpaceWaCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    cout << "3 4\n";
    return 0;
}
'@

$innerWa = Submit-Code $innerSpaceWaCode
$innerWa
Wait-Submission $innerWa.submission_id
```

### 8.2 预期结果

```text
status = WRONG_ANSWER
score = 0
```

### 8.3 说明

当前 default-trim-checker 不忽略行内空格差异。

因此：

```text
actual:   "3 4\n"
expected: "3\n"
```

应判定为：

```text
WRONG_ANSWER
```

验收结论：

```text
行内空格 WA 通过
```

---

## 九、CE 验收

### 9.1 测试代码

```powershell
$ceCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    cout << 3 << '\n'
    return 0;
}
'@

$ce = Submit-Code $ceCode
$ce
Wait-Submission $ce.submission_id
```

### 9.2 预期结果

```text
status = COMPILE_ERROR
score = 0
cases = []
```

### 9.3 文件检查

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($ce.submission_id)\build\compile.log" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($ce.submission_id)\result.json" -Encoding UTF8
```

预期：

```text
compile.log 中有 g++ 编译错误
result.json.status = COMPILE_ERROR
result.json.cases = []
```

验收结论：

```text
COMPILE_ERROR 通过
```

---

## 十、RE 验收

### 10.1 测试代码

```powershell
$reCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    return 1;
}
'@

$re = Submit-Code $reCode
$re
Wait-Submission $re.submission_id
```

### 10.2 预期结果

```text
status = RUNTIME_ERROR
score = 0
message = process exited with code 1
```

### 10.3 文件检查

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($re.submission_id)\result.json" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($re.submission_id)\cases\001\checker.log" -Encoding UTF8
```

预期：

```text
checker.log = process exited with code 1
```

不应混入大量 nsjail `[I] Mount ...` 日志。

验收结论：

```text
RUNTIME_ERROR 通过
```

---

## 十一、TLE 验收

### 11.1 测试代码

```powershell
$tleCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    while (true) {}
    return 0;
}
'@

$tle = Submit-Code $tleCode
$tle
Wait-Submission $tle.submission_id 15
```

### 11.2 预期结果

```text
status = TIME_LIMIT_EXCEEDED
score = 0
message = time limit exceeded
```

### 11.3 文件检查

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($tle.submission_id)\result.json" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($tle.submission_id)\cases\001\checker.log" -Encoding UTF8
```

预期：

```text
checker.log = time limit exceeded
```

验收结论：

```text
TIME_LIMIT_EXCEEDED 通过
```

---

## 十二、防读取答案验收

### 12.1 测试代码

```powershell
$hackAnsCode = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    ifstream fin("/data/ojos/problems/2-a-plus-b/tests/001.ans");
    if (fin.good()) {
        string s;
        getline(fin, s);
        cout << s << '\n';
    } else {
        cout << "NO_ANSWER_VISIBLE\n";
    }
    return 0;
}
'@

$hack = Submit-Code $hackAnsCode
$hack
Wait-Submission $hack.submission_id
```

### 12.2 预期结果

```text
stdout.txt = NO_ANSWER_VISIBLE
status = WRONG_ANSWER
```

重点不是该提交 AC，而是确认：

```text
用户程序不能读取 /data/ojos/problems/.../*.ans
```

### 12.3 文件检查

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\$($hack.submission_id)\cases\001\stdout.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\$($hack.submission_id)\result.json" -Encoding UTF8
```

验收结论：

```text
防读取答案通过
```

---

## 十三、Cancel 验收

### 13.1 创建待取消提交

```powershell
$cancelTarget = Submit-Code $waCode
$cancelTarget
Wait-Submission $cancelTarget.submission_id
```

### 13.2 执行 cancel

```powershell
$cancelObj = @{
  reason = "test cancel"
}

$json = $cancelObj | ConvertTo-Json -Compress
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)

Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/submissions/$($cancelTarget.submission_id)/cancel" `
  -ContentType "application/json; charset=utf-8" `
  -Headers $headers `
  -Body $bytes
```

### 13.3 查询结果

```powershell
Show-Submission $cancelTarget.submission_id
```

预期：

```text
status = CANCELLED
cancel_reason = test cancel
cancelled_at 非空
```

验收结论：

```text
Cancel 单份提交通过
```

---

## 十四、Rejudge 覆盖 CANCELLED 验收

### 14.1 执行 rejudge

```powershell
Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/problems/2/rejudge" `
  -Headers $headers
```

预期响应：

```text
problem_id = 2
enqueued > 0
```

### 14.2 检查被 cancel 的提交

```powershell
Wait-Submission $cancelTarget.submission_id 15
```

预期：

```text
status 不再是 CANCELLED
status 变为重新评测后的实际结果
```

说明：

```text
rejudge problem 的语义是重测该题全部提交，包括 CANCELLED
```

验收结论：

```text
Rejudge 覆盖 CANCELLED 通过
```

---

## 十五、Cases API 验收

### 15.1 请求

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/20/cases" `
  -Headers $headers
```

### 15.2 预期响应

应返回类似：

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

说明：

```text
Cases API 当前从 result.json 读取 cases
不再查询 submission_cases
```

验收结论：

```text
/submissions/:id/cases 通过
```

---

## 十六、Result 文件验收

### 16.1 检查 result.json

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\result.json" -Encoding UTF8
```

AC 示例：

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

验收点：

```text
submission_id 正确
status 正确
score 正确
cases 数组正确
stdout_path 正确
stderr_path 正确
checker_log_path 正确
```

验收结论：

```text
result.json 落盘通过
```

---

## 十七、Submission 文件结构验收

### 17.1 检查目录

```powershell
Get-ChildItem "D:\Untitled-OJ\storage\submissions\20" -Recurse
```

预期包含：

```text
source/main.cpp
build/main
build/compile.log
build/compile.stdout.log
build/compile.stderr.log
cases/001/stdin.txt
cases/001/stdout.txt
cases/001/stderr.txt
cases/001/checker.log
result.json
```

验收结论：

```text
Submission Storage 通过
```

---

## 十八、题目包读取验收

### 18.1 检查题目包

```powershell
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\problem.yaml" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\tests\cases.yaml" -Encoding UTF8
Get-ChildItem "D:\Untitled-OJ\storage\problems\2-a-plus-b\tests" -Recurse
```

预期：

```text
problem.yaml 存在
tests/cases.yaml 存在
tests/001.in 存在
tests/001.ans 存在
```

`problem.yaml` 中应包含：

```yaml
tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml
```

`cases.yaml` 中应使用：

```yaml
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
```

不再使用：

```yaml
no: 0
```

验收结论：

```text
Problem Package 读取通过
```

---

## 十九、Redis Stream 验收

### 19.1 查看 Worker 日志

```powershell
docker logs ojos-judge-worker --tail 200
```

预期判题时出现：

```text
received judge stream message
submission claimed
judge finished
judge stream message acked
```

### 19.2 查看 XPENDING

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

预期：

```text
0
```

验收结论：

```text
Redis Stream 消费和 XACK 通过
```

---

## 二十、PostgreSQL 验收

### 20.1 查看 submissions

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

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

预期：

```text
status 与 result.json 一致
code_path 非空
result_path 非空
judged_at 对已完成提交非空
```

当前：

```text
memory_kb = 0
```

是已知限制，不算验收失败。

---

## 二十一、nsjail 安全验收

### 21.1 进入 worker 容器

```powershell
docker exec -it ojos-judge-worker bash
```

### 21.2 手动验证 jail 内不可见 problems

容器内执行：

```bash
mkdir -p /tmp/jailtest
chmod 777 /tmp/jailtest

nsjail --mode o \
  --user 10001 \
  --group 10001 \
  --disable_clone_newuser \
  --time_limit 2 \
  --cwd /work \
  --chroot /jail/root \
  --bindmount_ro /bin:/bin \
  --bindmount_ro /lib:/lib \
  --bindmount_ro /lib64:/lib64 \
  --bindmount_ro /usr:/usr \
  --bindmount_ro /etc/alternatives:/etc/alternatives \
  --bindmount_ro /dev/null:/dev/null \
  --bindmount_ro /dev/zero:/dev/zero \
  --bindmount_ro /dev/urandom:/dev/urandom \
  --bindmount /tmp/jailtest:/work \
  --tmpfsmount /tmp \
  -- /bin/bash -lc 'id && ls /data/ojos/problems || echo no-problems-visible && touch /work/write-test && echo write-ok'
```

预期：

```text
uid=10001 gid=10001 groups=10001
no-problems-visible
write-ok
```

验收结论：

```text
nsjail 基础隔离通过
```

---

## 二十二、已发现并修复的问题记录

本轮 Judge 验收过程中曾发现并修复以下问题：

```text
1. problem.yaml 中 tests.cases 路径拼接错误，曾出现 tests/tests/cases.yaml
2. nsjail 参数顺序错误，--user / --group 曾被放到 -- 后面
3. C++ 编译时 g++ 找不到 ld
4. compile.log 初期为空
5. run.command 中 {exe} 未替换，导致 runtime code 127
6. stdout.txt 初期为空
7. 旧 stdout/stderr 文件权限导致重定向失败
8. RE message 曾混入 nsjail [I] 日志
9. CE 日志初期捕获不稳定
```

对应修复：

```text
1. tests.cases 相对 package_dir，不再重复拼 tests.root
2. nsjail 参数全部放在 -- 前
3. 使用 /usr/bin/g++，并加 -B/usr/bin/
4. 编译日志改为 jail 内文件重定向
5. command 和 args 都执行占位符替换
6. 运行改为 jail 内 stdin/stdout/stderr 文件重定向
7. case 运行前删除旧 stdout.txt / stderr.txt / checker.log
8. RE message 只保留 exit line 和用户 stderr 摘要
9. compile.stdout.log / compile.stderr.log 合并为 compile.log
```

---

## 二十三、当前验收矩阵

| 项目                      | 状态  | 说明                               |
| ----------------------- | --- | -------------------------------- |
| judge-api 创建提交          | 通过  | 返回 PENDING、code_path、result_path |
| Redis Stream XADD       | 通过  | worker 能收到消息                     |
| judge-worker XREADGROUP | 通过  | 能消费任务                            |
| try_claim_submission    | 通过  | 能原子 PENDING -> JUDGING           |
| problem.yaml 读取         | 通过  | 能读取 package_dir                  |
| tests/cases.yaml 读取     | 通过  | 不再拼错路径                           |
| nsjail 编译               | 通过  | C++ 编译成功                         |
| nsjail 运行               | 通过  | 能运行用户程序                          |
| AC                      | 通过  | score = 100                      |
| WA                      | 通过  | checker.log 显示 expected / actual |
| CE                      | 通过  | compile.log 有报错                  |
| RE                      | 通过  | message 干净                       |
| TLE                     | 通过  | TIME_LIMIT_EXCEEDED              |
| Trim Checker            | 通过  | 忽略末尾空格和空行                        |
| 行内空格差异                  | 通过  | 判 WA                             |
| 防读取 answer              | 通过  | 输出 NO_ANSWER_VISIBLE             |
| cancel                  | 通过  | status = CANCELLED               |
| rejudge                 | 通过  | 覆盖 CANCELLED 并重测                 |
| cases API               | 通过  | 从 result.json 读取                 |
| result.json             | 通过  | 正常落盘                             |
| memory_kb               | 未完成 | 当前为 0                            |
| 多语言完整验收                 | 未完成 | 后续验证 c11 / python3 / java17      |

---

## 二十四、当前结论

当前 Judge 子系统已经完成传统题主链路验收。

可确认：

```text
judge-api 可用
judge-worker 可用
Redis Streams 队列可用
Problem Package 读取可用
Submission Storage 可用
nsjail 基础隔离可用
default-trim-checker 可用
default-sum-scorer 可用
result.json 可用
cancel / rejudge 可用
```

当前系统已经可以完成：

```text
用户提交代码
源码文件落盘
Redis Stream 排队
worker 消费任务
读取题目包
沙箱编译
沙箱运行
checker 判定
scorer 汇总
result.json 落盘
数据库摘要更新
查询提交结果
查询 case 结果
```

下一阶段不应继续反复重构主链路，而应优先做：

```text
1. 多语言验收：c11 / python3 / java17
2. memory_kb cgroup v2 统计
3. 输出大小限制
4. checker 抽象
5. scorer 抽象
6. runner 抽象
```
