# Submission Storage 文档

## 一、模块定位

Submission Storage 是 OJOS Judge 子系统中用于保存提交源码、编译产物、测试点运行输出、checker 日志和完整评测结果的文件存储结构。

当前 OJOS 不再把用户源码正文和每个测试点结果完整写入数据库。

当前设计是：

```text
数据库保存摘要
文件系统保存完整产物
```

也就是：

```text
submissions 表：
    保存 status / score / time_ms / memory_kb / message / code_path / result_path

storage/submissions：
    保存 source / build / cases / result.json
```

这样可以避免：

```text
数据库存大源码
数据库存大量 stdout / stderr / checker log
数据库存完整 case result
重测时反复写大字段
后续导出评测产物困难
```

当前提交文件根目录：

```text
storage/submissions/
```

在容器内挂载为：

```text
/data/ojos/submissions/
```

---

## 二、整体目录结构

每份提交都有独立目录：

```text
storage/submissions/{submission_id}/
```

例如：

```text
storage/submissions/20/
```

推荐结构：

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
│       ├── main
│       ├── stdin.txt
│       ├── stdout.txt
│       ├── stderr.txt
│       └── checker.log
│
└── result.json
```

不同语言的源码文件名由：

```text
services/judge-worker/config/languages.yaml
```

决定。

例如：

```text
cpp17  -> source/main.cpp
c11    -> source/main.c
python3 -> source/main.py
java17 -> source/Main.java
```

---

## 三、容器内路径与宿主机路径

宿主机路径：

```text
D:\Untitled-OJ\storage\submissions\
```

容器内路径：

```text
/data/ojos/submissions/
```

例如 submission 20：

宿主机：

```text
D:\Untitled-OJ\storage\submissions\20\
```

容器内：

```text
/data/ojos/submissions/20/
```

数据库中保存的是容器内路径：

```text
/data/ojos/submissions/20/source/main.cpp
/data/ojos/submissions/20/result.json
```

原因：

```text
judge-api / judge-worker 都运行在容器内
容器内路径对服务可直接访问
```

前端和外部用户不应直接依赖宿主机路径。

---

## 四、source 目录

`source/` 用于保存用户提交的原始源码。

示例：

```text
storage/submissions/20/source/main.cpp
```

容器内路径：

```text
/data/ojos/submissions/20/source/main.cpp
```

数据库字段：

```text
submissions.code_path
```

指向该文件。

当前不再使用：

```text
submissions.code
```

保存源码正文。

---

### 4.1 source 目录示例

```text
source/

└── main.cpp
```

`main.cpp` 示例：

```cpp
#include <bits/stdc++.h>
using namespace std;

int main() {
    long long a, b;
    cin >> a >> b;
    cout << a + b << '\n';
    return 0;
}
```

---

### 4.2 code_sha256

数据库字段：

```text
submissions.code_sha256
```

保存源码文件内容的 SHA-256。

用途：

```text
校验源码文件完整性
辅助排查重复提交
辅助后续缓存编译结果
辅助审计
```

当前它不是唯一约束，也不表示相同代码一定只判一次。

---

## 五、build 目录

`build/` 用于保存编译阶段文件。

编译型语言会使用该目录，例如：

```text
cpp17
cpp20
c11
java17
```

示例：

```text
storage/submissions/20/build/
```

结构：

```text
build/

├── main
├── main.cpp
├── compile.log
├── compile.stdout.log
└── compile.stderr.log
```

说明：

| 文件                   | 说明              |
| -------------------- | --------------- |
| `main.cpp`           | 复制到 build 目录的源码 |
| `main`               | 编译得到的可执行文件      |
| `compile.stdout.log` | 编译 stdout       |
| `compile.stderr.log` | 编译 stderr       |
| `compile.log`        | 合并后的编译日志        |

---

### 5.1 为什么源码会复制到 build 目录

源码原始位置是：

```text
source/main.cpp
```

编译时会复制到：

```text
build/main.cpp
```

原因：

```text
编译在 nsjail 内执行
build 目录被挂载为 jail 内 /work
编译器只需要看到 /work 下的源码和输出文件
```

编译阶段 jail 内视角：

```text
/work/main.cpp
/work/main
/work/compile.stdout.log
/work/compile.stderr.log
```

---

### 5.2 compile.log

`compile.log` 是面向查询和排错的合并日志。

示例：

```text
sandbox_status: Ok
sandbox_message:

[stdout]


[stderr]
```

编译错误时示例：

```text
sandbox_status: RuntimeError
sandbox_message: process exited with code 1

[stdout]

[stderr]
/work/main.cpp: In function 'int main()':
/work/main.cpp:6:22: error: expected ';' before 'return'
```

当前规则：

```text
compile.stdout.log 和 compile.stderr.log 在 jail 内生成
compile.log 在 worker 中合并生成
```

不要依赖父进程 FD 捕获编译日志。

---

### 5.3 编译产物权限

编译产物需要可执行权限。

对于 C/C++：

```text
build/main
```

后续会被复制到每个 case 目录：

```text
cases/001/main
```

复制后应设置：

```text
0755
```

否则运行时可能出现：

```text
process exited with code 126
```

即 permission denied。

---

## 六、cases 目录

`cases/` 用于保存每个测试点的运行产物。

每个 case 一个独立目录：

```text
cases/{case_no:03}/
```

例如：

```text
cases/001/
cases/002/
cases/003/
```

`case_no` 来自题目包：

```text
tests/cases.yaml
```

示例：

```yaml
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: 100
    group: 0
    sample: false
    hidden: true
```

对应目录：

```text
cases/001/
```

---

## 七、单个 case 目录结构

典型结构：

```text
cases/001/

├── main
├── stdin.txt
├── stdout.txt
├── stderr.txt
└── checker.log
```

说明：

| 文件            | 说明                 |
| ------------- | ------------------ |
| `main`        | 当前 case 使用的可执行文件副本 |
| `stdin.txt`   | 从题目输入复制来的标准输入      |
| `stdout.txt`  | 用户程序标准输出           |
| `stderr.txt`  | 用户程序标准错误           |
| `checker.log` | checker 判定日志       |

对于解释型语言，可能没有 `main`，而是复制或引用源码文件，例如：

```text
main.py
```

具体由 runner 和 language config 决定。

---

## 八、stdin.txt

`stdin.txt` 是 worker 从题目包输入文件复制得到的输入副本。

题目包输入文件：

```text
storage/problems/{id}-{slug}/tests/001.in
```

提交 case 输入副本：

```text
storage/submissions/{submission_id}/cases/001/stdin.txt
```

jail 内路径：

```text
/work/stdin.txt
```

运行时通过重定向传给用户程序：

```bash
/work/main < /work/stdin.txt > /work/stdout.txt 2> /work/stderr.txt
```

说明：

```text
用户程序只看到 stdin.txt
用户程序看不到题目包中的原始 input
用户程序即使改写 stdin.txt，也不会影响题目包
```

---

## 九、stdout.txt

`stdout.txt` 是用户程序标准输出。

jail 内路径：

```text
/work/stdout.txt
```

宿主机路径示例：

```text
D:\Untitled-OJ\storage\submissions\20\cases\001\stdout.txt
```

容器内路径示例：

```text
/data/ojos/submissions/20/cases/001/stdout.txt
```

AC 示例：

```text
3
```

该文件会被 checker 读取，并与题目答案比较。

---

## 十、stderr.txt

`stderr.txt` 是用户程序标准错误。

jail 内路径：

```text
/work/stderr.txt
```

用途：

```text
记录运行时错误信息
辅助调试 RUNTIME_ERROR
辅助显示用户程序 stderr
```

如果程序无 stderr 输出，该文件为空是正常的。

---

## 十一、checker.log

`checker.log` 是 checker 判定日志。

AC 示例：

```text
accepted
```

WA 示例：

```text
expected: 3
actual: 0
```

RE 示例：

```text
process exited with code 1
```

TLE 示例：

```text
time limit exceeded
```

说明：

```text
checker.log 面向调试和后续后台查看
比赛环境下不一定直接展示给普通用户
```

是否展示应由 contest feedback policy 决定。

---

## 十二、result.json

`result.json` 是一份提交的完整评测结果。

路径：

```text
storage/submissions/{submission_id}/result.json
```

容器内路径：

```text
/data/ojos/submissions/{submission_id}/result.json
```

数据库字段：

```text
submissions.result_path
```

指向该文件。

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

`GET /judge/submissions/:id/cases` 当前从 `result.json` 读取 `cases` 数组。

---

## 十三、数据库字段关系

`submissions` 表保存文件路径和摘要。

核心字段：

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

| 字段            | 对应文件                                   |
| ------------- | -------------------------------------- |
| `code_path`   | `storage/submissions/{id}/source/*`    |
| `result_path` | `storage/submissions/{id}/result.json` |
| `code_sha256` | `source/*` 内容哈希                        |
| `status`      | `result.json.status` 的摘要               |
| `score`       | `result.json.score` 的摘要                |
| `time_ms`     | `result.json.time_ms` 的摘要              |
| `memory_kb`   | `result.json.memory_kb` 的摘要            |
| `message`     | `result.json.message` 的摘要              |

说明：

```text
数据库是查询入口
result.json 是完整结果
case 文件是原始运行产物
```

---

## 十四、为什么不再使用 submission_cases

旧设计使用：

```text
submission_cases
```

保存每个测试点结果。

当前已经废弃。

原因：

```text
case 结果可能很大
stdout / stderr / checker log 不适合进数据库
后续子任务 / 捆绑点 / SPJ / 交互题结果结构会更复杂
result.json 更适合保存完整树状结构
数据库只需要可索引摘要
```

当前接口：

```http
GET /judge/submissions/:id/cases
```

不再查询 `submission_cases`，而是：

```text
读取 submissions.result_path
解析 result.json
返回 cases
```

---

## 十五、为什么不再使用 submissions.code

旧设计使用：

```text
submissions.code TEXT
```

保存源码。

当前已经废弃。

原因：

```text
源码可能较大
数据库不适合保存大量代码正文
编译需要实际文件
后续下载源码 / 审计 / 编译缓存都更适合文件化
```

当前使用：

```text
submissions.code_path
submissions.code_sha256
```

管理源码。

---

## 十六、创建提交时的文件写入流程

`judge-api` 创建提交时的文件流程：

```text
收到请求
  ↓
检查用户权限
  ↓
插入 submissions 初始记录
  ↓
创建 storage/submissions/{id}
  ↓
创建 source/
  ↓
根据 language 写 source/main.*
  ↓
计算 code_sha256
  ↓
创建初始 result.json
  ↓
更新 submissions.code_path / code_sha256 / result_path
  ↓
XADD Redis Stream
```

初始 `result.json` 可以类似：

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

---

## 十七、Worker 判题时的文件写入流程

`judge-worker` 判题时的文件流程：

```text
try_claim_submission
  ↓
读取 code_path
  ↓
读取 problem.package_dir
  ↓
读取 problem.yaml
  ↓
读取 tests/cases.yaml
  ↓
创建 build/
  ↓
复制 source 到 build/
  ↓
nsjail 编译
  ↓
写 compile.stdout.log / compile.stderr.log / compile.log
  ↓
对每个 case:
      创建 cases/{case_no:03}/
      删除旧 stdout.txt / stderr.txt / checker.log
      写 stdin.txt
      复制可执行文件
      nsjail 运行
      生成 stdout.txt / stderr.txt
      checker 比较 answer
      写 checker.log
  ↓
生成 result.json
  ↓
更新 submissions 摘要
```

---

## 十八、Rejudge 时的文件语义

Rejudge 某题时：

```text
judge-api 将该题全部 submissions 状态重置为 PENDING
judge-api 清空 score / time / memory / message / judged_at / cancel 信息
judge-api 重新投递 Redis Stream 任务
worker 重新读取原 code_path
worker 覆盖 build / cases / result.json
```

当前语义：

```text
rejudge 覆盖旧评测结果
```

不是：

```text
保留每次评测版本
```

也就是说，当前不维护：

```text
submission version
judge run version
historical result snapshots
```

这是开发阶段刻意选择。

如果后续需要审计历史，可另行设计：

```text
judge_runs
storage/submissions/{id}/runs/{run_id}
```

当前不做。

---

## 十九、Cancel 时的文件语义

Cancel 单份提交时：

```text
submissions.status = CANCELLED
submissions.cancelled_at = now
submissions.cancelled_by = current_user
submissions.cancel_reason = reason
```

Cancel 不删除：

```text
source/
build/
cases/
result.json
```

原因：

```text
取消成绩不是删除提交
仍然需要保留审计和复查依据
rejudge 时可以复用 code_path
```

Rejudge 会覆盖 cancel 状态，并重新评测该提交。

---

## 二十、文件权限注意事项

当前 worker 和 nsjail 的权限模型：

```text
worker 进程通常以容器 root 运行
用户程序在 nsjail 内以 uid/gid 10001 运行
```

因此需要注意：

```text
由 worker 创建的文件可能是 root:root
由用户程序创建的文件可能是 10001:10001
```

关键规则：

```text
运行 case 前删除旧 stdout.txt / stderr.txt / checker.log
让 uid=10001 在 jail 内重新创建输出文件
stdin.txt 由 worker 写入，只需要用户程序可读
可执行文件复制后需要 chmod 755
```

如果不删除旧输出文件，可能出现：

```text
bash 重定向 stdout.txt 失败
process exited with code 1
stdout.txt 为空
```

---

## 二十一、路径格式规则

当前 `result.json` 中保存的是容器内路径，例如：

```text
/data/ojos/submissions/20/cases/001/stdout.txt
```

而不是宿主机路径：

```text
D:\Untitled-OJ\storage\submissions\20\cases\001\stdout.txt
```

原因：

```text
服务运行在容器内
数据库和 result.json 面向服务内部路径
```

如果需要在宿主机调试，可以按挂载规则转换：

```text
/data/ojos/submissions/20
    ↓
D:\Untitled-OJ\storage\submissions\20
```

后续如果支持对象存储，路径可能演变为：

```text
storage://submissions/20/result.json
s3://bucket/submissions/20/result.json
```

因此不要在前端硬编码本地路径。

---

## 二十二、调试命令

### 22.1 查看某份提交目录

```powershell
Get-ChildItem "D:\Untitled-OJ\storage\submissions\20" -Recurse
```

---

### 22.2 查看源码

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\source\main.cpp" -Encoding UTF8
```

---

### 22.3 查看编译日志

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\build\compile.log" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\build\compile.stdout.log" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\build\compile.stderr.log" -Encoding UTF8
```

---

### 22.4 查看 case 文件

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\stdin.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\stdout.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\stderr.txt" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\submissions\20\cases\001\checker.log" -Encoding UTF8
```

---

### 22.5 查看 result.json

```powershell
Get-Content "D:\Untitled-OJ\storage\submissions\20\result.json" -Encoding UTF8
```

---

### 22.6 查看数据库记录

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
    code_sha256,
    result_path,
    judged_at,
    cancelled_at,
    cancel_reason
FROM submissions
WHERE id = 20;
```

---

## 二十三、Git 管理规则

不应提交真实提交产物：

```text
storage/submissions/*
```

可以保留：

```text
storage/submissions/.gitkeep
```

`.gitignore` 应包含：

```gitignore
storage/submissions/*
!storage/submissions/.gitkeep
```

原因：

```text
提交源码可能包含用户隐私
提交结果会快速膨胀
build 和 cases 是运行产物
result.json 是运行产物
```

题目包中的开发样例是否提交，需要单独按项目策略决定。

---

## 二十四、当前已知限制

当前 Submission Storage 仍有以下限制：

```text
不保存历史 rejudge 版本
不保存 judge run version
不支持对象存储
不支持结果压缩
不支持过期清理
不支持大文件输出限制
不支持按用户隔离存储配额
不支持冷热数据分层
```

当前开发阶段可以接受。

后续如果提交量变大，需要设计：

```text
submission artifact retention policy
result cleanup job
object storage adapter
per-user quota
per-contest artifact policy
```

---

## 二十五、当前结论

当前 Submission Storage 已经完成从：

```text
数据库保存源码和 case 结果
```

到：

```text
源码 / 编译产物 / case 输出 / result.json 文件化存储
```

的升级。

当前设计使 Judge 子系统具备：

```text
更清晰的数据库边界
更方便的编译运行文件模型
更完整的评测产物留存
更适合后续 SPJ / 子任务 / 交互题扩展的结果结构
```

当前核心原则是：

```text
数据库存摘要
文件系统存完整产物
result.json 存完整结构化结果
```

后续扩展 runner / checker / scorer 时，应优先扩展：

```text
result.json
storage/submissions/{id}/cases/
```

而不是重新把复杂结果塞回数据库。
