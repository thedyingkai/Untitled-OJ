# OJOS 文档索引

## 一、文档定位

本文档是 OJOS 项目的文档入口。

OJOS 当前文档已经从早期的单文件模块说明，重构为按领域拆分的文档结构。

当前推荐文档结构：

```text
docs/

├── index.md
├── architecture.md
├── database.md
├── deployment.md
│
├── problem/
│   └── package-format.md
│
├── judge/
│   ├── overview.md
│   ├── api.md
│   ├── worker.md
│   ├── sandbox.md
│   ├── submission-storage.md
│   ├── result-format.md
│   └── validation.md
│
├── changelog/
│   └── judge-nsjail-pipeline.md
docs/permission/
└── overview.md
```

旧文档中的以下入口已经废弃：

```text
architecture_overview.md
judge_module.md
judge_worker_module.md
```

对应的新文档为：

```text
architecture_overview.md  -> docs/architecture.md
judge_module.md           -> docs/judge/overview.md
judge_worker_module.md    -> docs/judge/worker.md
```

---

## 二、当前项目状态

OJOS 当前已经完成从早期 MVP 到可运行核心原型的阶段升级。

当前已经完成的核心能力：

```text
Gateway 统一入口
Auth 登录注册
JWT 鉴权
Gateway 可信用户上下文透传
Shared 公共基础库
Permission Core 资源级权限系统
Problem API 基础 CRUD
Problem Package 文件化题目包
Judge API 提交 / 查询 / cancel / rejudge
Redis Streams Judge Queue
Rust Judge Worker
nsjail 沙箱编译和运行
Submission 文件化存储
result.json 完整评测结果
default-trim-checker
default-sum-scorer
PostgreSQL 数据库迁移
Jaeger 链路追踪
Docker Compose 本地部署
```

当前已经验证：

```text
AC
WA
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
末尾空格和末尾空行忽略
行内空格不同判 WRONG_ANSWER
用户程序无法读取题目答案
cancel 单份提交
rejudge 重测包括 CANCELLED
/submissions/:id/cases 从 result.json 读取
Redis Stream 消息消费和 XACK
```

当前仍未完成：

```text
memory_kb cgroup v2 统计
多语言完整验收
输出大小限制
SPJ
子任务 / 捆绑点
交互题
通信题
提交答案题
Contest Core
Scoreboard Core
Module Registry
Launcher
```

---

## 三、推荐阅读顺序

### 3.1 只想了解项目整体

先读：

```text
README.md
docs/index.md
docs/architecture.md
```

说明：

```text
README.md              项目入口和当前状态总览
docs/index.md          文档导航
docs/architecture.md   系统总体架构
```

---

### 3.2 要启动本地环境

读：

```text
docs/deployment.md
docs/database.md
```

说明：

```text
docs/deployment.md     Docker Compose、端口、日志、启动、重建、Jaeger、Redis
docs/database.md       PostgreSQL 表结构、migration、常用 SQL
```

---

### 3.3 要理解题目存储

读：

```text
docs/problem/package-format.md
```

说明：

```text
docs/problem/package-format.md
```

记录：

```text
storage/problems/{id}-{slug}
problem.yaml
tests/cases.yaml
tests/groups.yaml
statement/
tutorial/
checker/
runner/
scorer/
```

---

### 3.4 要理解 Judge

按顺序读：

```text
docs/judge/overview.md
docs/judge/api.md
docs/judge/worker.md
docs/judge/sandbox.md
docs/judge/submission-storage.md
docs/judge/result-format.md
docs/judge/validation.md
```

说明：

```text
overview.md             Judge 总览
api.md                  Judge HTTP API
worker.md               Judge Worker 执行流程
sandbox.md              nsjail 沙箱
submission-storage.md   提交文件存储结构
result-format.md        result.json 格式
validation.md           当前验收测试记录
```

---

### 3.5 要继续开发 Judge

重点读：

```text
docs/judge/worker.md
docs/judge/sandbox.md
docs/judge/result-format.md
docs/problem/package-format.md
docs/database.md
```

尤其注意：

```text
不要恢复 test_cases
不要恢复 submission_cases
不要恢复 submissions.code
不要重新引入 NATS
不要裸跑用户代码
不要让用户程序看到 /data/ojos/problems
```

### 3.6 要理解权限系统：

```text
docs/permission/overview.md
docs/database.md
```

---

## 四、核心文档说明

## 4.1 `docs/architecture.md`

系统总体架构文档。

内容包括：

```text
OJOS 项目定位
Kernel + Modules 思想
Gateway / Auth / Problem API / Judge API / Judge Worker 分工
PostgreSQL / Redis / Jaeger 关系
文件存储模型
Judge 数据流
权限架构
题型架构方向
赛制架构方向
后续模块化方向
```

适合用于：

```text
理解整体系统
确认模块边界
规划后续开发
避免过早抽象
```

---

## 4.2 `docs/database.md`

数据库文档。

内容包括：

```text
users
roles
user_roles
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
problems
submissions
schema_migrations
```

并明确废弃：

```text
test_cases
submission_cases
submissions.code
```

适合用于：

```text
写 migration
排查数据库状态
确认表结构
理解 Permission Core
理解 problem / submission 与文件系统关系
```

---

## 4.3 `docs/deployment.md`

部署文档。

内容包括：

```text
Docker Compose 服务
端口规划
环境变量
volume 挂载
PostgreSQL
Redis
Jaeger
judge-worker nsjail capability
启动命令
重建命令
日志命令
Redis Streams 调试
NATS 清理检查
常见部署问题
```

适合用于：

```text
启动环境
重启服务
排查容器
排查 Jaeger
排查 Redis Stream
排查 judge-worker
```

---

## 4.4 `docs/problem/package-format.md`

题目包格式文档。

内容包括：

```text
storage/problems/{id}-{slug}
problem.yaml
statement/
tests/
tests/cases.yaml
tests/groups.yaml
checker/
runner/
scorer/
tutorial/
```

重点规则：

```text
tests.cases 是相对 package_dir 的路径
case.input / case.answer 是相对 tests.root 的路径
case_no 从 1 开始
不再使用 no: 0
```

适合用于：

```text
开发 problem-api
开发题目导入导出
排查 load cases.yaml failed
设计 SPJ / 子任务 / 捆绑点
```

---

## 五、Judge 文档说明

## 5.1 `docs/judge/overview.md`

Judge 模块总览。

内容包括：

```text
judge-api 和 judge-worker 分工
Redis Streams 队列
题目包读取
submission 文件化存储
数据库边界
状态流转
PENDING 扫描
原子抢任务
checker / scorer
当前验收结果
当前限制
```

适合用于：

```text
整体理解 Judge 子系统
确认 Judge 与 Problem 的边界
确认当前能力和限制
```

---

## 5.2 `docs/judge/api.md`

Judge API 文档。

当前接口：

```http
POST /judge/submissions
GET  /judge/submissions/:id
GET  /judge/submissions/:id/cases
POST /judge/submissions/:id/cancel
POST /judge/problems/:id/rejudge
```

通过 Gateway 访问：

```http
POST /api/judge/submissions
GET  /api/judge/submissions/:id
GET  /api/judge/submissions/:id/cases
POST /api/judge/submissions/:id/cancel
POST /api/judge/problems/:id/rejudge
```

已废弃接口：

```http
POST /judge/problems
POST /judge/test-cases
```

适合用于：

```text
前端对接
接口测试
PowerShell 调试
权限检查确认
```

---

## 5.3 `docs/judge/worker.md`

Judge Worker 文档。

内容包括：

```text
Redis Stream 消费
Consumer Group
XREADGROUP
XACK
PENDING 扫描
try_claim_submission
problem.yaml 读取
tests/cases.yaml 读取
nsjail 编译
nsjail 运行
checker
scorer
result.json 写入
submissions 摘要更新
常见问题
```

适合用于：

```text
开发 judge-worker
排查 PENDING
排查 Redis Stream
排查编译运行
排查 result.json
```

---

## 5.4 `docs/judge/sandbox.md`

nsjail 沙箱文档。

内容包括：

```text
为什么使用 nsjail
Docker capability
nsjail 参数
uid/gid 10001
/work 隔离
题目答案保护
时间限制
内存限制
进程数限制
文件描述符限制
输出大小限制计划
C++ / Java / Python 注意事项
安全验收用例
```

适合用于：

```text
排查 nsjail
确认用户程序不能读 ans
修编译环境
修运行环境
后续加 cgroup v2
```

---

## 5.5 `docs/judge/submission-storage.md`

提交文件存储文档。

内容包括：

```text
storage/submissions/{submission_id}
source/
build/
cases/
result.json
code_path
code_sha256
result_path
cancel 文件语义
rejudge 文件语义
Git 忽略规则
```

适合用于：

```text
排查提交目录
查看源码
查看编译日志
查看 stdout / stderr / checker.log
理解数据库和文件系统关系
```

---

## 5.6 `docs/judge/result-format.md`

评测结果格式文档。

内容包括：

```text
result.json 顶层结构
cases 数组结构
AC 示例
WA 示例
CE 示例
RE 示例
TLE 示例
SYSTEM_ERROR 示例
CANCELLED 示例
数据库摘要同步规则
message 截断规则
后续 subtask / bundle 扩展方向
```

适合用于：

```text
开发 cases API
开发前端提交详情页
开发 scorer
设计子任务结果格式
```

---

## 5.7 `docs/judge/validation.md`

Judge 验收文档。

内容包括：

```text
AC 测试
WA 测试
CE 测试
RE 测试
TLE 测试
Trim Checker 测试
防读取答案测试
cancel 测试
rejudge 测试
cases API 测试
Redis Stream 测试
nsjail 安全测试
```

适合用于：

```text
回归测试
重构后验收
提交前自查
排查 Judge 主链路
```

---

## 六、已废弃旧文档

以下旧文档不应继续作为主文档维护：

```text
docs/architecture_overview.md
docs/judge_module.md
docs/judge_worker_module.md
```

对应迁移关系：

| 旧文档                             | 新文档                      |
| ------------------------------- | ------------------------ |
| `docs/architecture_overview.md` | `docs/architecture.md`   |
| `docs/judge_module.md`          | `docs/judge/overview.md` |
| `docs/judge_worker_module.md`   | `docs/judge/worker.md`   |

旧文档可以暂时保留作为历史资料，但应在顶部标注：

```text
Deprecated
```

或者移动到：

```text
docs/legacy/
```

推荐最终处理方式：

```text
docs/legacy/architecture_overview.md
docs/legacy/judge_module.md
docs/legacy/judge_worker_module.md
```

不要继续在 README 或 docs/index.md 中引用旧文档。

---

## 七、当前架构关键变化

当前文档重构对应以下架构变化：

```text
NATS -> Redis Streams
DB test_cases -> Problem Package
DB submission_cases -> result.json
submissions.code -> code_path + source file
裸跑用户代码 -> nsjail sandbox
Judge 管题目 -> Problem API 管题目
Judge Worker 读数据库测试点 -> Judge Worker 读 problem.yaml / cases.yaml
```

这些变化是当前主线，不要回退。

---

## 八、当前推荐开发顺序

当前后续开发建议：

```text
1. 完成文档重构
2. 多语言验收：c11 / python3 / java17
3. memory_kb 接入 cgroup v2 统计
4. 输出大小限制
5. checker 抽象
6. scorer 抽象
7. runner 抽象
8. Problem Core / Dataset Core 深化
9. Contest Core
10. Scoreboard Core
11. Permission API
12. Module Registry
13. Launcher
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

## 九、提交前文档检查

提交代码前建议检查：

```powershell
cd D:\Untitled-OJ

git status -uall
```

确认文档新增：

```text
docs/index.md
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

如果保留旧文档，应确认它们不再被索引引用：

```powershell
Select-String `
  -Path .\README.md,.\docs\*.md,.\docs\judge\*.md,.\docs\problem\*.md `
  -Pattern "architecture_overview|judge_module|judge_worker_module"
```

预期：

```text
没有主文档引用旧文档
```

如果有引用，应改为新路径。

---

## 十、文档与代码一致性检查

### 10.1 检查 NATS 是否残留

```powershell
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期：

```text
无输出
```

---

### 10.2 检查旧 DB 表是否残留

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

### 10.3 检查 submissions 字段

```sql
SELECT column_name
FROM information_schema.columns
WHERE table_name = 'submissions'
ORDER BY ordinal_position;
```

应有：

```text
code_path
code_sha256
result_path
cancelled_at
cancelled_by
cancel_reason
```

不应有：

```text
code
```

---

### 10.4 检查 Judge Worker 构建

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo check
cargo build
```

---

### 10.5 检查 Go 服务构建

```powershell
cd D:\Untitled-OJ\services\shared
go build ./...

cd D:\Untitled-OJ\services\auth
go build .

cd D:\Untitled-OJ\services\gateway
go build .

cd D:\Untitled-OJ\services\problem-api
go build .

cd D:\Untitled-OJ\services\judge-api
go build .
```

---

## 十一、文档维护原则

后续维护文档时遵循：

```text
README.md 写总览，不堆实现细节
docs/index.md 写导航，不写长篇实现
docs/architecture.md 写整体架构
docs/database.md 写数据库
docs/deployment.md 写部署
docs/problem/* 写题目领域
docs/judge/* 写 Judge 领域
docs/changelog/* 写重大变更记录
```

不要把所有内容继续塞进一个：

```text
judge_module.md
```

否则后续会重新变成垃圾桶文档。

---

## 十二、当前结论

当前文档体系已经从旧的：

```text
单文件模块说明
```

重构为：

```text
按领域拆分的架构文档
```

当前最重要的文档入口是：

```text
README.md
docs/index.md
docs/architecture.md
```

当前最重要的 Judge 文档是：

```text
docs/judge/overview.md
docs/judge/api.md
docs/judge/worker.md
docs/judge/sandbox.md
docs/judge/submission-storage.md
docs/judge/result-format.md
docs/judge/validation.md
```

当前最重要的 Problem 文档是：

```text
docs/problem/package-format.md
```

当前最重要的基础设施文档是：

```text
docs/database.md
docs/deployment.md
```

后续新增功能时，应优先把文档放到对应领域目录，而不是继续堆到 README。
