# Judge nsjail Pipeline 重构记录

## 一、变更定位

本文档记录 OJOS Judge 子系统从早期 MVP 判题链路重构为当前稳定主线的过程。

本次重构核心目标：

```text
废弃旧数据库测试点模型
废弃 NATS 判题消息链路
废弃容器内裸跑用户程序
引入题目包文件化
引入 Redis Streams 可靠任务队列
引入 nsjail 沙箱
引入 submission 文件化存储
引入 result.json 完整结果文件
完善 cancel / rejudge 语义
```

重构后的 Judge 主线为：

```text
Problem Package
+
Judge API
+
Redis Streams
+
Judge Worker
+
nsjail
+
Submission Storage
+
result.json
```

---

## 二、重构前旧架构

旧架构大致为：

```text
judge-api
  ↓
数据库 problems / test_cases
  ↓
NATS submission.created
  ↓
judge-worker
  ↓
读取 test_cases
  ↓
直接在 worker 容器内运行用户代码
  ↓
写 submissions / submission_cases
```

旧架构存在的问题：

```text
NATS Core Pub/Sub 不适合作为可靠判题队列
worker 不在线时消息可能丢失
没有 ACK
没有 Pending List
没有 Consumer Group
测试点 input/output 放数据库不适合扩展
submission_cases 结构不适合复杂结果
用户源码正文放 submissions.code 不合理
用户程序裸跑在 worker 容器内不安全
用户程序可能读取题目答案
用户程序可能覆写测试数据
cancel / rejudge 语义不清晰
题目数据和 Judge 耦合过重
```

因此旧架构必须重构。

---

## 三、本次删除的内容

本次重构删除或废弃：

```text
NATS
NATS_URL
async-nats
futures-util
judge-worker/src/event.rs

test_cases
submission_cases
submissions.code

POST /judge/problems
POST /judge/test-cases

数据库测试点读取逻辑
submission_cases 写入逻辑
worker 容器内裸跑用户程序
legacy test_cases 兼容逻辑
```

注意：

```text
当前项目仍处于开发阶段，不维护旧格式兼容。
```

因此不需要保留：

```text
旧 test_cases
旧 submission_cases
旧 no: 0 cases.yaml
旧 submissions.code
旧 NATS 事件
```

---

## 四、本次新增的内容

本次重构新增：

```text
Redis Streams Judge Queue
Redis Consumer Group: judge-workers
PostgreSQL PENDING 兜底扫描
数据库原子抢任务
Problem Package
problems.package_dir
tests/cases.yaml
storage/submissions
submissions.code_path
submissions.code_sha256
submissions.result_path
result.json
nsjail 沙箱编译
nsjail 沙箱运行
default-trim-checker
default-sum-scorer
cancel 单份提交
rejudge 某题全部提交
/submissions/:id/cases 从 result.json 读取
```

当前 Judge Queue：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

当前用户程序运行隔离：

```text
uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
```

---

## 五、重构后的整体数据流

当前提交链路：

```text
User
  ↓
Gateway
  ↓
judge-api
  ↓
检查 Gateway 注入用户上下文
  ↓
检查 judge.submit @ system:0
  ↓
读取 problem.package_dir
  ↓
创建 submissions 记录
  ↓
写入 storage/submissions/{id}/source/main.*
  ↓
写入初始 result.json
  ↓
XADD ojos:judge:submissions
  ↓
judge-worker XREADGROUP
  ↓
try_claim_submission: PENDING -> JUDGING
  ↓
读取 submissions.code_path
  ↓
读取 problems.package_dir
  ↓
读取 problem.yaml
  ↓
读取 tests/cases.yaml
  ↓
nsjail 编译
  ↓
nsjail 按 case 运行
  ↓
default-trim-checker
  ↓
default-sum-scorer
  ↓
写 stdout.txt / stderr.txt / checker.log
  ↓
写 result.json
  ↓
更新 submissions 摘要
  ↓
XACK Redis Stream 消息
```

---

## 六、题目数据变化

### 6.1 旧模式

旧模式：

```text
test_cases 表保存 input / output / score
judge-worker 从数据库读取测试点
```

问题：

```text
测试数据可能很大
input/output 不适合存 TEXT
不适合多文件测试
不适合子任务 / 捆绑点 / SPJ
不适合导入导出
```

### 6.2 新模式

新模式：

```text
storage/problems/{id}-{slug}/
```

题目包结构：

```text
problem.yaml
tests/cases.yaml
tests/groups.yaml
tests/*.in
tests/*.ans
statement/
tutorial/
runner/
checker/
scorer/
```

数据库只保存：

```text
problems.package_dir
```

Judge Worker 通过 `package_dir` 读取题目包。

核心规则：

```text
tests.cases 是相对 package_dir 的路径
case.input / case.answer 是相对 tests.root 的路径
case_no 从 1 开始
不使用 no: 0
```

---

## 七、提交数据变化

### 7.1 旧模式

旧模式：

```text
submissions.code 保存源码正文
submission_cases 保存每个测试点摘要
```

问题：

```text
源码正文不适合塞数据库
case stdout / stderr / checker log 不适合塞数据库
后续复杂结果结构无法表达
rejudge 结果覆盖和版本关系不清晰
```

### 7.2 新模式

新模式：

```text
storage/submissions/{submission_id}/
```

提交目录：

```text
storage/submissions/{submission_id}/

├── source/
│   └── main.*
├── build/
│   ├── main
│   ├── compile.log
│   ├── compile.stdout.log
│   └── compile.stderr.log
├── cases/
│   └── 001/
│       ├── stdin.txt
│       ├── stdout.txt
│       ├── stderr.txt
│       └── checker.log
└── result.json
```

数据库保存：

```text
submissions.code_path
submissions.code_sha256
submissions.result_path
submissions.status
submissions.score
submissions.time_ms
submissions.memory_kb
submissions.message
```

完整结果保存在：

```text
result.json
```

---

## 八、队列变化

### 8.1 旧模式：NATS

旧模式使用：

```text
NATS Core Pub/Sub
```

问题：

```text
不持久化
没有 ACK
worker 不在线可能丢消息
不适合判题这种可靠任务
```

### 8.2 新模式：Redis Streams

新模式使用：

```text
Redis Streams
```

当前 Stream：

```text
ojos:judge:submissions
```

当前 Consumer Group：

```text
judge-workers
```

优势：

```text
消息持久化
Consumer Group
ACK
Pending List
积压可观测
多 worker 竞争消费
```

但最终可靠性仍依赖：

```text
PostgreSQL PENDING 扫描
+
数据库原子抢任务
```

原因：

```text
PostgreSQL 是事实源
Redis Streams 是实时任务队列
```

---

## 九、沙箱变化

### 9.1 旧模式

旧模式：

```text
用户程序直接在 worker 容器内执行
```

风险：

```text
用户程序可能读取题目答案
用户程序可能覆写测试数据
用户程序可能访问 worker 文件系统
用户程序可能访问网络
用户程序可能影响其他提交
```

### 9.2 新模式

新模式：

```text
nsjail
```

当前隔离：

```text
用户程序 uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
用户程序不能读取 *.ans
用户程序不能覆盖题目 *.in / *.ans
每个 case 独立运行目录
```

Docker Compose 不使用：

```text
privileged: true
```

而使用最小 capability：

```text
SYS_ADMIN
SYS_CHROOT
SETUID
SETGID
NET_ADMIN
```

---

## 十、Checker 变化

当前实现默认 checker：

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

已验证：

```text
"3\n"        vs "3\n" -> ACCEPTED
"3   \n\n"  vs "3\n" -> ACCEPTED
"3 4\n"     vs "3\n" -> WRONG_ANSWER
```

后续可扩展：

```text
strict checker
ignore-whitespace checker
float checker
special judge
interactive checker
```

---

## 十一、Scorer 变化

当前实现默认 scorer：

```text
default-sum-scorer
```

规则：

```text
case AC 得 case.score
case 非 AC 得 0
总分为所有 case 得分之和
全部 AC -> ACCEPTED
否则按失败状态汇总
```

当前传统题可用。

后续需要扩展：

```text
ACM scorer
OI scorer
IOI scorer
Subtask scorer
Bundle scorer
Heuristic scorer
```

---

## 十二、Cancel 语义

当前 cancel 语义：

```text
取消某一份提交的成绩
```

不是：

```text
删除提交
删除源码
删除 result.json
删除 case 输出
```

Cancel 会更新：

```text
submissions.status = CANCELLED
submissions.cancelled_at = now
submissions.cancelled_by = current_user
submissions.cancel_reason = reason
```

Cancel 权限：

```text
problem.manage.data @ problem:{problem_id}
```

其中 `problem_id` 来自该 submission 所属题目。

---

## 十三、Rejudge 语义

当前 rejudge 语义：

```text
重测某题全部提交
```

包括：

```text
CANCELLED
```

也就是说：

```text
rejudge 会覆盖 cancel 状态
```

Rejudge 会：

```text
将该题全部 submissions 重置为 PENDING
清空 score / time_ms / memory_kb / message
清空 judged_at
清空 cancelled_at / cancelled_by / cancel_reason
重新 XADD 每个 submission_id
```

当前不维护：

```text
submission version
judge run version
历史 result 快照
```

开发阶段直接覆盖旧结果。

---

## 十四、API 变化

当前 Judge API：

```http
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
POST /judge/submissions/:id/cancel
POST /judge/problems/:id/rejudge
```

通过 Gateway：

```http
POST /api/judge/submissions
GET  /api/judge/submissions/:id
GET  /api/judge/submissions/:id/cases
POST /api/judge/submissions/:id/cancel
POST /api/judge/problems/:id/rejudge
```

已删除旧接口：

```http
POST /judge/problems
POST /judge/test-cases
```

原因：

```text
题目和测试数据管理属于 problem-api
judge-api 不再管理题目数据
```

---

## 十五、数据库变化

当前保留：

```text
problems
submissions
```

当前删除或废弃：

```text
test_cases
submission_cases
submissions.code
```

`problems` 新重点字段：

```text
package_dir
```

`submissions` 新重点字段：

```text
code_path
code_sha256
result_path
cancelled_at
cancelled_by
cancel_reason
judged_at
```

当前数据库边界：

```text
problems 保存题目元数据和 package_dir
submissions 保存提交摘要和文件路径
result.json 保存完整 case 结果
storage/problems 保存题目包
storage/submissions 保存提交产物
```

---

## 十六、已验证功能

本次重构后已经验证：

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

典型 AC 结果：

```text
status = ACCEPTED
score = 100
stdout.txt = 3
checker.log = accepted
```

典型 WA 结果：

```text
status = WRONG_ANSWER
checker.log = expected / actual
```

典型防读 ans 结果：

```text
stdout.txt = NO_ANSWER_VISIBLE
```

说明用户程序无法看到：

```text
/data/ojos/problems
```

---

## 十七、修复过的问题

本次重构过程中修复过的问题：

```text
problem.yaml 中 tests.cases 路径重复拼接，导致 tests/tests/cases.yaml
nsjail 参数顺序错误，--user / --group 被放到 -- 后
C++ 编译找不到 ld
compile.log 为空
run.command 中 {exe} 未替换，导致 code 127
stdout.txt 为空
旧 stdout / stderr 文件权限导致重定向失败
RE message 混入大量 nsjail [I] 日志
cases API 需要从 result.json 读取
rejudge 没有覆盖 CANCELLED
```

对应修复：

```text
tests.cases 相对 package_dir
case.input / case.answer 相对 tests.root
nsjail 参数全部放在 -- 前
C++ 编译加入 -B/usr/bin/
编译日志改为 jail 内文件重定向
command 和 args 都做占位符替换
运行改为 jail 内 stdin/stdout/stderr 文件重定向
case 运行前删除旧 stdout.txt / stderr.txt / checker.log
RE message 只保留摘要
rejudge 选择该题全部 submissions，包括 CANCELLED
```

---

## 十八、当前已知限制

当前仍未完成：

```text
memory_kb 真实统计
cgroup v2 memory peak
输出大小限制
stderr 大小限制
compile.log 大小限制
多语言完整验收
SPJ
子任务
捆绑点
交互题
通信题
提交答案题
启发式题
XAUTOCLAIM 回收 Redis 长 pending 消息
Redis Stream XTRIM
JUDGING 超时恢复
```

当前：

```text
memory_kb = 0
```

是已知限制，不是本次验收失败。

---

## 十九、后续计划

后续推荐顺序：

```text
1. 多语言验收：c11 / python3 / java17
2. cgroup v2 memory_kb 统计
3. stdout / stderr / compile log 输出大小限制
4. XAUTOCLAIM 回收 Redis Stream pending
5. Redis Stream XTRIM
6. JUDGING 超时恢复
7. checker 抽象
8. scorer 抽象
9. runner 抽象
10. Problem Core / Dataset Core 深化
11. SPJ
12. 子任务 / 捆绑点
13. 交互题 / 通信题 / 提交答案题
```

当前不建议立刻做：

```text
Contest Core
Scoreboard Core
Module Registry
Launcher
```

原因：

```text
Problem / Dataset / Runner / Checker / Scorer 还需要继续稳定
```

---

## 二十、相关文档

本次重构后相关文档：

```text
docs/architecture.md
docs/database.md
docs/deployment.md

docs/problem/package-format.md

docs/judge/overview.md
docs/judge/api.md
docs/judge/worker.md
docs/judge/sandbox.md
docs/judge/submission-storage.md
docs/judge/result-format.md
docs/judge/validation.md
```

旧文档应废弃或移动到：

```text
docs/legacy/
```

包括：

```text
docs/architecture_overview.md
docs/judge_module.md
docs/judge_worker_module.md
```

---

## 二十一、提交前检查

提交前建议检查：

```powershell
cd D:\Untitled-OJ

git status -uall
```

检查 NATS 是否残留：

```powershell
Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

检查旧文档引用：

```powershell
Select-String `
  -Path .\README.md,.\docs\*.md,.\docs\judge\*.md,.\docs\problem\*.md,.\docs\changelog\*.md `
  -Pattern "architecture_overview|judge_module|judge_worker_module"
```

主文档不应再引用旧文档作为当前设计。

检查构建：

```powershell
cd D:\Untitled-OJ\services\judge-worker
cargo fmt
cargo check
cargo build
```

```powershell
cd D:\Untitled-OJ\services\judge-api
go build .
```

```powershell
cd D:\Untitled-OJ\services\problem-api
go build .
```

---

## 二十二、当前结论

本次重构完成后，OJOS Judge 子系统已经从：

```text
数据库测试点
+
NATS 事件
+
裸跑用户代码
+
submission_cases
```

升级为：

```text
Problem Package
+
Redis Streams
+
PostgreSQL PENDING 兜底扫描
+
数据库原子抢任务
+
nsjail Sandbox
+
Submission Storage
+
result.json
```

当前 Judge 主链路已经可以稳定完成：

```text
提交
排队
消费
抢任务
读取题目包
沙箱编译
沙箱运行
checker
scorer
结果落盘
数据库摘要更新
查询
cancel
rejudge
```

这标志着 OJOS Judge 从“能跑的 MVP”进入：

```text
可继续扩展的安全隔离判题核心
```

下一阶段应围绕：

```text
多语言
内存统计
输出限制
Runner / Checker / Scorer 抽象
```

继续推进，而不是反复推翻当前主链路。
