# Judge 模块总览

## 一、模块定位

Judge 模块是 OJOS 的核心评测模块，负责完成从“用户提交代码”到“返回评测结果”的完整链路。

当前 Judge 模块已经从早期 MVP 形态升级为：

```text
Package-based Judge Pipeline
+
Redis Streams Reliable Queue
+
nsjail Sandbox Runner
+
File-based Submission Result
```

它的核心职责是：

```text
接收提交
创建判题任务
可靠投递任务
安全执行用户代码
读取题目包测试数据
运行测试点
执行 checker
执行 scorer
写入 result.json
更新提交摘要
提供提交结果查询
```

当前 Judge 模块不负责：

```text
创建题目
管理题面
管理测试数据
维护题解
维护题目包结构
```

这些已经迁移到：

```text
problem-api
```

Judge 模块只依赖 `problems.package_dir` 读取题目包，不再直接管理题目数据。

---

## 二、当前模块组成

当前 Judge 子系统由两个服务组成：

```text
services/judge-api
services/judge-worker
```

以及一组文件存储目录：

```text
storage/submissions
```

模块关系如下：

```text
Client
  ↓
Gateway
  ↓
judge-api
  ↓
Redis Stream: ojos:judge:submissions
  ↓
judge-worker
  ↓
storage/submissions/{submission_id}
  ↓
PostgreSQL submissions 摘要更新
```

---

## 三、judge-api 职责

`judge-api` 是 Judge 模块的 HTTP API 层。

路径：

```text
services/judge-api
```

当前职责：

```text
创建提交
查询提交摘要
查询提交 case 结果
取消单份提交成绩
重测某题全部提交
读取 Gateway 注入的可信用户上下文
检查 judge.submit 权限
检查 problem.manage.data 权限
读取 problems.package_dir
将用户源码写入 storage/submissions
创建 result.json 初始文件
写入 submissions 摘要
向 Redis Stream 投递判题任务
```

当前 `judge-api` 不负责：

```text
创建题目
添加测试点
维护题面
维护题解
维护 checker / runner / scorer 配置
执行代码
比较输出
保存每个 case 的数据库记录
```

以下旧接口已经废弃：

```http
POST /judge/problems
POST /judge/test-cases
```

题目与测试数据管理统一由：

```text
problem-api
```

负责。

---

## 四、judge-worker 职责

`judge-worker` 是 Rust 实现的后台判题进程。

路径：

```text
services/judge-worker
```

它不是 HTTP 服务，不监听业务端口。

当前职责：

```text
连接 PostgreSQL
连接 Redis
加载 languages.yaml
确保 Redis Consumer Group 存在
启动时扫描 PENDING submissions
定时扫描 PENDING submissions
通过 XREADGROUP 消费 Redis Stream
解析 submission_id
执行 try_claim_submission
读取 submissions.code_path
读取 problems.package_dir
加载 problem.yaml
加载 tests/cases.yaml
使用 nsjail 编译用户代码
使用 nsjail 按 case 运行用户代码
执行 default-trim-checker
执行 default-sum-scorer
写入 stdout.txt / stderr.txt / checker.log
写入 result.json
更新 submissions 摘要
XACK Redis Stream 消息
```

当前 Worker 的可靠性模型为：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
+
Redis XACK
```

---

## 五、当前 Judge 队列

Judge 任务队列使用 Redis Streams。

当前 Stream：

```text
ojos:judge:submissions
```

当前 Consumer Group：

```text
judge-workers
```

生产者：

```text
judge-api
```

消费者：

```text
judge-worker
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

使用 Redis Streams 的原因：

```text
Judge 任务需要持久化
Judge 任务需要 ACK
Judge 任务需要 Pending List
Judge 任务需要 Consumer Group
Judge 任务需要多 worker 竞争消费
Judge 任务需要失败恢复
```

当前已经不再使用：

```text
NATS
NATS_URL
async-nats
NATS Core Pub/Sub
```

---

## 六、当前题目数据来源

Judge Worker 不再从数据库 `test_cases` 表读取测试点。

当前题目数据来自题目包：

```text
storage/problems/{id}-{slug}/
```

Judge Worker 通过数据库字段：

```text
problems.package_dir
```

定位题目包目录。

题目包入口文件：

```text
problem.yaml
```

测试点清单：

```text
tests/cases.yaml
```

当前核心约定：

```text
tests.cases 是相对 package_dir 的路径，例如 tests/cases.yaml
tests.root 是测试数据根目录，例如 tests
case.input / case.answer 是相对 tests.root 的路径
case_no 从 1 开始
不使用 no: 0
```

示例结构：

```text
storage/problems/2-a-plus-b/

├── problem.yaml
├── statement/
│   └── zh-cn.md
├── tests/
│   ├── cases.yaml
│   ├── groups.yaml
│   ├── 001.in
│   └── 001.ans
├── checker/
│   └── checker.yaml
├── runner/
│   └── runner.yaml
├── scorer/
│   └── scorer.yaml
└── tutorial/
    ├── zh-cn.md
    └── std.cpp
```

---

## 七、当前提交数据存储

提交源码和完整评测结果不再直接存在数据库中。

当前提交目录：

```text
storage/submissions/{submission_id}/
```

示例结构：

```text
storage/submissions/{submission_id}/

├── source/
│   └── main.cpp
│
├── build/
│   ├── main
│   ├── compile.log
│   ├── compile.stdout.log
│   └── compile.stderr.log
│
├── cases/
│   └── 001/
│       ├── stdin.txt
│       ├── stdout.txt
│       ├── stderr.txt
│       └── checker.log
│
└── result.json
```

数据库 `submissions` 表只保存摘要：

```text
id
problem_id
user_id
language
status
score
time_ms
memory_kb
message
code_path
code_sha256
result_path
judged_at
cancelled_at
cancelled_by
cancel_reason
created_at
updated_at
```

其中：

```text
code_path
```

指向用户源码文件。

```text
result_path
```

指向完整评测结果文件。

完整 case 结果从：

```text
result.json
```

读取，不再写入 `submission_cases` 表。

---

## 八、当前数据库边界

当前 Judge 相关数据库表重点是：

```text
problems
submissions
```

其中：

```text
problems
```

保存题目元数据和题目包路径。

```text
submissions
```

保存提交摘要、状态、源码路径、结果路径和 cancel 信息。

当前已经删除或废弃：

```text
test_cases
submission_cases
submissions.code
```

废弃原因：

```text
测试点数据已经文件化
用户源码已经文件化
完整 case 结果已经文件化
数据库只保留可查询摘要和索引字段
```

---

## 九、当前评测状态

当前 Judge 支持的提交状态包括：

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

核心状态流转：

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

## 十、原子抢任务模型

Worker 即使从 Redis Stream 收到任务，也不能直接判题。

必须先执行数据库原子抢任务：

```sql
UPDATE submissions
SET status = 'JUDGING', updated_at = NOW()
WHERE id = $1 AND status = 'PENDING'
RETURNING id;
```

如果返回一行：

```text
当前 worker 抢到任务
```

如果返回零行：

```text
任务已经被其他 worker 抢走
任务已经判完
任务已经被 cancel
任务不是 PENDING
```

则跳过该任务并 ACK Redis 消息。

这个机制用于防止：

```text
Redis 历史消息重复投递
PENDING 扫描和 Redis 消费同时处理同一 submission
多个 worker 竞争同一任务
rejudge 后旧消息干扰
```

---

## 十一、PENDING 兜底扫描

即使使用 Redis Streams，也必须保留数据库 PENDING 扫描。

原因：

```text
judge-api 可能 INSERT submission 成功但 XADD 失败
Redis 可能短暂不可用
worker 可能消费后崩溃
worker 可能判题中途退出
旧版本可能遗留 PENDING
手动修复数据后需要恢复
```

因此可靠模型是：

```text
PostgreSQL 是最终事实来源
Redis Streams 是实时任务队列
PENDING 扫描是兜底恢复机制
try_claim_submission 是防重复执行机制
```

Worker 当前有两类扫描：

```text
启动时扫描 PENDING
运行中定时扫描 PENDING
```

扫描发现 PENDING 后直接尝试 claim。

如果 claim 失败，说明已经被其他路径处理，跳过即可。

---

## 十二、当前沙箱模型

Judge Worker 当前使用：

```text
nsjail
```

执行编译和运行。

当前已经实现的隔离边界：

```text
用户程序运行在独立 mount namespace
用户程序运行在独立 pid namespace
用户程序运行在独立 ipc namespace
用户程序运行在独立 uts namespace
用户程序运行在独立 net namespace
用户程序 uid/gid = 10001
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
用户程序无法读取题目 *.ans
用户程序无法覆盖题目 *.in / *.ans
每个测试点独立运行目录
运行时 stdin/stdout/stderr 通过 /work 内文件重定向
```

Docker Compose 中 `judge-worker` 不使用：

```text
privileged: true
```

但需要最小 capability：

```text
SYS_ADMIN
SYS_CHROOT
SETUID
SETGID
NET_ADMIN
```

当前已验证：

```text
jail 内 /data/ojos/problems 不存在
jail 内 uid=10001 gid=10001
/work 可写
用户程序读取 /data/ojos/problems/.../*.ans 会失败
```

当前限制：

```text
memory_kb 暂未采集
内存限制当前主要通过 rlimit_as 实现
后续应接入 cgroup v2 统计峰值内存
```

---

## 十三、当前 Checker / Scorer

当前已经实现基础默认 checker：

```text
default-trim-checker
```

规则：

```text
忽略每行末尾空格和 Tab
忽略末尾空行
不忽略行内空格差异
不忽略多余非空行
```

已验证：

```text
输出 "3\n" 对 expected "3\n" AC
输出 "3   \n\n" 对 expected "3\n" AC
输出 "3 4\n" 对 expected "3\n" WA
```

当前已经实现基础默认 scorer：

```text
default-sum-scorer
```

规则：

```text
每个测试点按 cases.yaml 中 score 字段给分
AC 得该测试点分数
非 AC 得 0
总分为所有 case 得分之和
如果全部 case AC，最终状态 ACCEPTED
如果存在 WA，最终状态 WRONG_ANSWER
如果存在 CE / RE / TLE / SYSTEM_ERROR，按失败状态汇总
```

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

## 十四、当前 Runner

当前主要支持：

```text
traditional-runner
```

流程：

```text
编译用户代码
逐个测试点运行
stdin 由测试点 input 提供
stdout 与 answer 交给 checker
stderr 保存到 case 目录
checker.log 保存 checker 信息
```

当前不支持：

```text
interactive-runner
communication-runner
output-only-runner
heuristic-runner
```

这些后续应通过 Runner Core 扩展，而不是把所有逻辑硬塞进当前 traditional runner。

---

## 十五、当前权限接入

当前 Judge API 已接入 Permission Core。

当前检查点：

```text
POST /judge/submissions
    -> judge.submit @ system:0

POST /judge/submissions/:id/cancel
    -> problem.manage.data @ problem:{problem_id}

POST /judge/problems/:id/rejudge
    -> problem.manage.data @ problem:{problem_id}
```

也就是说：

```text
提交代码需要 judge.submit
取消某份提交需要管理该提交所属题目的数据权限
重测某题全部提交需要管理该题数据权限
```

后续还应补充：

```text
submission.view.own
submission.view.all
submission.rejudge
submission.cancel
submission.delete
```

比赛环境下还需要考虑：

```text
反馈策略
封榜策略
题目可见性
比赛参赛权限
管理员查看权限
```

---

## 十六、当前 API 概览

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

详细 API 说明见：

```text
docs/judge/api.md
```

---

## 十七、当前验收结果

当前已经完成以下功能验收：

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

典型已验证结果：

```text
AC:
    status = ACCEPTED
    score = 100
    checker.log = accepted

WA:
    status = WRONG_ANSWER
    checker.log 显示 expected / actual

TLE:
    status = TIME_LIMIT_EXCEEDED
    message = time limit exceeded

防读取答案:
    用户程序读取 /data/ojos/problems/.../*.ans 失败
    stdout = NO_ANSWER_VISIBLE

Cancel:
    status = CANCELLED
    cancel_reason 正常保存

Rejudge:
    CANCELLED 提交重新进入 PENDING 并被重测
```

详细验收记录见：

```text
docs/judge/validation.md
```

---

## 十八、当前已知限制

当前 Judge 模块仍有以下限制：

```text
memory_kb 暂未统计
内存限制主要依赖 rlimit_as
输出大小限制尚未完善
多语言只完成基础配置，仍需逐项验收
checker 插件化尚未完成
runner 插件化尚未完成
scorer 插件化尚未完成
交互题尚未支持
通信题尚未支持
提交答案题尚未支持
启发式评分尚未支持
比赛反馈策略尚未支持
封榜策略尚未支持
submission 查询权限还需要结合 own/all 和 contest policy 继续细化
```

这些限制不影响当前传统题主链路验收，但会影响后续生产化和比赛系统开发。

---

## 十九、后续演进方向

Judge 模块后续应拆分和深化为：

```text
judge-core
runner-core
checker-core
scorer-core
dataset-core
problem-type modules
contest feedback policy
```

推荐演进顺序：

```text
1. 多语言验收：c11 / python3 / java17
2. memory_kb 接入 cgroup v2 统计
3. 输出大小限制
4. checker 抽象：default / special judge / float
5. scorer 抽象：ACM / OI / IOI / Subtask / Bundle
6. runner 抽象：traditional / interactive / communication / output-only
7. Problem Core / Dataset Core 深化
8. 比赛反馈策略
9. contest-core
10. scoreboard-core
```

当前不建议立刻把 contest 和 scoreboard 接上 Judge 结果，因为：

```text
runner / checker / scorer 抽象还没有稳定
result.json 格式还需要随子任务和捆绑点扩展
比赛反馈策略还没有定义
```

先稳定 Judge 结果模型，再接 Contest。

---

## 二十、当前结论

Judge 模块当前已经完成 OJOS 的核心判题闭环：

```text
登录
鉴权
提交
源码落盘
排队
消费
题目包读取
nsjail 编译
nsjail 运行
checker
scorer
result.json
数据库摘要更新
查询
cancel
rejudge
```

当前 Judge 链路已经从早期不可靠和不安全的形态升级为：

```text
Redis Streams Consumer Group
+
PostgreSQL PENDING 扫描
+
数据库原子抢任务
+
nsjail Sandbox
+
File-based Submission Result
```

这使 Judge 模块具备了继续演进为完整 OJOS Judge Core 的基础。

当前下一步重点不是继续堆业务，而是稳定：

```text
多语言执行
内存统计
Runner 抽象
Checker 抽象
Scorer 抽象
Result 格式
Problem / Dataset 模型
```

这些稳定后，才能可靠支持：

```text
OI
NOI
ACM
IOI
启发式算法题
子任务
捆绑点
交互题
通信题
提交答案题
滚榜
封榜
气球
打印
ICPC Tools 兼容
```
