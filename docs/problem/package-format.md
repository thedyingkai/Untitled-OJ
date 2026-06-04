# Problem Package Format 文档

## 一、模块定位

Problem Package 是 OJOS 中题目数据的文件化存储格式。

当前 OJOS 不再把测试点输入输出直接存入数据库表：

```text
test_cases
```

也不再让 Judge Worker 从数据库读取测试点内容。

当前设计是：

```text
Problem API 管理题目包
Judge Worker 读取题目包
数据库只保存题目元数据和 package_dir
```

题目包的核心目标是：

```text
题面文件化
测试数据文件化
题解文件化
runner / checker / scorer 配置文件化
方便导入导出
方便后续支持 Polygon / Lemon / Hydro / FPS 等格式转换
方便后续支持 SPJ / 子任务 / 捆绑点 / 交互题
```

当前题目包根目录：

```text
storage/problems/{id}-{slug}/
```

容器内路径：

```text
/data/ojos/problems/{id}-{slug}/
```

数据库字段：

```text
problems.package_dir
```

指向该题目包目录。

---

## 二、目录结构

推荐题目包结构：

```text
storage/problems/{id}-{slug}/

├── problem.yaml
│
├── statement/
│   ├── zh-cn.md
│   └── assets/
│
├── tests/
│   ├── cases.yaml
│   ├── groups.yaml
│   ├── 001.in
│   ├── 001.ans
│   ├── 002.in
│   └── 002.ans
│
├── checker/
│   └── checker.yaml
│
├── runner/
│   └── runner.yaml
│
├── scorer/
│   └── scorer.yaml
│
└── tutorial/
    ├── zh-cn.md
    └── std.cpp
```

示例：

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

## 三、命名规则

题目包目录名推荐：

```text
{id}-{slug}
```

例如：

```text
2-a-plus-b
```

其中：

| 部分     | 说明               |
| ------ | ---------------- |
| `id`   | 数据库中的 problem id |
| `slug` | 题目短标识，用于路径和 URL  |

推荐 slug 规则：

```text
小写字母
数字
连字符 -
不使用空格
不使用中文
不使用特殊符号
```

例如：

```text
a-plus-b
two-sum
shortest-path
matrix-query
```

实际目录可以带 id 前缀：

```text
2-a-plus-b
15-shortest-path
```

这样可以避免不同题目 slug 冲突。

---

## 四、problem.yaml

`problem.yaml` 是题目包入口文件。

Judge Worker 读取题目包时，首先读取：

```text
problem.yaml
```

示例：

```yaml
schema: ojos.problem.v1
id: 2
slug: 2-a-plus-b
title: A+B Problem
type: traditional
visibility: private
status: draft

limits:
  default:
    time_ms: 1000
    memory_mb: 256
  languages:
    cpp17:
      time_ms: 1000
      memory_mb: 256
    python3:
      time_ms: 2000
      memory_mb: 256

statement:
  default_locale: zh-cn
  files:
    zh-cn: statement/zh-cn.md
  assets_dir: statement/assets

runner:
  config: runner/runner.yaml

checker:
  config: checker/checker.yaml

scorer:
  config: scorer/scorer.yaml

tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml

tutorial:
  default_locale: zh-cn
  files:
    zh-cn: tutorial/zh-cn.md
  std:
    language: cpp17
    path: tutorial/std.cpp

source:
  format: ojos
  fingerprint: ""
```

---

## 五、problem.yaml 字段说明

### 5.1 schema

```yaml
schema: ojos.problem.v1
```

表示题目包格式版本。

当前推荐：

```text
ojos.problem.v1
```

后续如果题目包格式发生不兼容变更，可以升级为：

```text
ojos.problem.v2
```

当前开发阶段暂不做旧格式兼容。

---

### 5.2 id

```yaml
id: 2
```

题目数据库 ID。

应与数据库：

```text
problems.id
```

一致。

---

### 5.3 slug

```yaml
slug: 2-a-plus-b
```

题目 slug。

一般与目录名一致。

---

### 5.4 title

```yaml
title: A+B Problem
```

题目标题。

用于展示和导出。

数据库中也会保存一份题目标题摘要。

---

### 5.5 type

```yaml
type: traditional
```

题目类型。

当前已验证：

```text
traditional
```

后续可能支持：

```text
interactive
communication
output-only
heuristic
```

当前 Judge Worker 主要支持：

```text
traditional-runner
```

---

### 5.6 visibility

```yaml
visibility: private
```

题目可见性。

可选值建议：

```text
private
public
hidden
contest-only
```

当前具体权限仍应以数据库和 Permission Core 为准。

题目包中的 visibility 主要用于导入导出和展示参考。

---

### 5.7 status

```yaml
status: draft
```

题目状态。

可选值建议：

```text
draft
ready
published
archived
```

当前具体状态仍应以数据库为准。

---

## 六、limits

`limits` 定义题目默认资源限制和按语言覆盖限制。

示例：

```yaml
limits:
  default:
    time_ms: 1000
    memory_mb: 256
  languages:
    cpp17:
      time_ms: 1000
      memory_mb: 256
    python3:
      time_ms: 2000
      memory_mb: 256
    java17:
      time_ms: 3000
      memory_mb: 512
```

字段说明：

| 字段                           | 说明        |
| ---------------------------- | --------- |
| `default.time_ms`            | 默认时间限制，毫秒 |
| `default.memory_mb`          | 默认内存限制，MB |
| `languages.<lang>.time_ms`   | 某语言时间限制   |
| `languages.<lang>.memory_mb` | 某语言内存限制   |

规则：

```text
如果存在语言专属限制，优先使用语言专属限制
否则使用 default 限制
```

例如：

```text
cpp17 使用 1000ms / 256MB
python3 使用 2000ms / 256MB
未配置语言使用 default 1000ms / 256MB
```

注意：

```text
当前 memory_kb 统计尚未完成
当前 memory_mb 主要用于 nsjail --rlimit_as
```

---

## 七、statement

`statement` 定义题面文件。

示例：

```yaml
statement:
  default_locale: zh-cn
  files:
    zh-cn: statement/zh-cn.md
    en-us: statement/en-us.md
  assets_dir: statement/assets
```

字段说明：

| 字段               | 说明      |
| ---------------- | ------- |
| `default_locale` | 默认语言    |
| `files`          | 各语言题面文件 |
| `assets_dir`     | 题面资源目录  |

推荐结构：

```text
statement/

├── zh-cn.md
├── en-us.md
└── assets/
    ├── image-1.png
    └── graph.svg
```

当前至少应支持：

```text
zh-cn
```

题面文件应使用 UTF-8 编码。

中文乱码通常是因为：

```text
请求体不是 UTF-8
PowerShell 未使用 UTF-8 bytes
服务写文件时编码不一致
文件读取时未指定 UTF-8
```

---

## 八、tests

`tests` 定义测试数据入口。

示例：

```yaml
tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml
```

字段说明：

| 字段       | 说明       |
| -------- | -------- |
| `root`   | 测试数据根目录  |
| `groups` | 测试点组配置文件 |
| `cases`  | 测试点列表文件  |

重要路径规则：

```text
tests.root 是相对 package_dir 的路径
tests.groups 是相对 package_dir 的路径
tests.cases 是相对 package_dir 的路径
case.input / case.answer 是相对 tests.root 的路径
```

也就是说，如果：

```yaml
tests:
  root: tests
  cases: tests/cases.yaml
```

那么 cases 文件路径是：

```text
package_dir/tests/cases.yaml
```

不是：

```text
package_dir/tests/tests/cases.yaml
```

不要把：

```text
tests.root + tests.cases
```

重复拼接。

这是一个非常重要的规则。

---

## 九、cases.yaml

`cases.yaml` 是测试点列表。

路径通常为：

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

多个测试点示例：

```yaml
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: 20
    group: 1
    sample: true
    hidden: false

  - case_no: 2
    input: 002.in
    answer: 002.ans
    score: 80
    group: 2
    sample: false
    hidden: true
```

字段说明：

| 字段        | 类型      | 必填 | 说明                 |
| --------- | ------- | -: | ------------------ |
| `case_no` | integer |  是 | 测试点编号，从 1 开始       |
| `input`   | string  |  是 | 输入文件，相对 tests.root |
| `answer`  | string  |  是 | 答案文件，相对 tests.root |
| `score`   | integer |  是 | 测试点分数              |
| `group`   | integer |  否 | 所属组或子任务            |
| `sample`  | boolean |  否 | 是否样例               |
| `hidden`  | boolean |  否 | 是否隐藏               |

当前要求：

```text
case_no 从 1 开始
不使用 no: 0
不兼容旧格式
```

旧格式：

```yaml
cases:
  - "no": 0
    input: 000.in
    answer: 000.ans
```

当前不再使用。

---

## 十、测试点文件命名

推荐测试点文件命名：

```text
001.in
001.ans
002.in
002.ans
003.in
003.ans
```

不推荐：

```text
000.in
000.ans
```

当前建议：

```text
case_no = 1 对应 001.in / 001.ans
case_no = 2 对应 002.in / 002.ans
```

这样可以保持：

```text
case_no
文件名
cases/{case_no:03}
```

三者一致。

例如：

```yaml
case_no: 1
input: 001.in
answer: 001.ans
```

对应：

```text
tests/001.in
tests/001.ans
storage/submissions/{id}/cases/001/
```

---

## 十一、groups.yaml

`groups.yaml` 用于描述测试点组、子任务或分组信息。

当前传统题可使用简单结构。

示例：

```yaml
groups:
  - group: 0
    name: default
    score: 100
    dependencies: []
```

未来 OI / IOI 子任务可扩展为：

```yaml
groups:
  - group: 1
    name: subtask-1
    score: 30
    dependencies: []

  - group: 2
    name: subtask-2
    score: 70
    dependencies: [1]
```

当前 default-sum-scorer 不强依赖复杂 group 信息。

后续 subtask / bundle scorer 会使用。

---

## 十二、runner 配置

`runner` 定义题目运行方式。

`problem.yaml` 中：

```yaml
runner:
  config: runner/runner.yaml
```

`runner/runner.yaml` 示例：

```yaml
type: traditional
name: traditional-runner
```

字段说明：

| 字段     | 说明        |
| ------ | --------- |
| `type` | 题目运行类型    |
| `name` | runner 名称 |

当前支持：

```text
traditional
```

后续可能支持：

```text
interactive
communication
output-only
heuristic
```

当前 Judge Worker 主要按 traditional runner 处理：

```text
编译
逐 case 运行
stdin/stdout
checker
scorer
```

---

## 十三、checker 配置

`checker` 定义输出比较方式。

`problem.yaml` 中：

```yaml
checker:
  config: checker/checker.yaml
```

`checker/checker.yaml` 示例：

```yaml
type: default-trim
name: default-trim-checker
```

当前已实现：

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

后续可扩展：

```text
default-strict-checker
ignore-whitespace-checker
float-checker
special-judge
interactive-checker
```

对于 SPJ，未来可能需要：

```yaml
type: special
language: cpp17
source: checker/spj.cpp
```

当前暂未实现。

---

## 十四、scorer 配置

`scorer` 定义分数汇总方式。

`problem.yaml` 中：

```yaml
scorer:
  config: scorer/scorer.yaml
```

`scorer/scorer.yaml` 示例：

```yaml
type: default-sum
name: default-sum-scorer
```

当前已实现：

```text
default-sum-scorer
```

规则：

```text
case AC 得 case.score
case 非 AC 得 0
总分为所有 case 分数之和
全部 AC -> ACCEPTED
否则按失败状态汇总
```

后续可扩展：

```text
acm-scorer
oi-scorer
ioi-scorer
subtask-scorer
bundle-scorer
heuristic-scorer
```

---

## 十五、tutorial

`tutorial` 定义题解与标准程序。

示例：

```yaml
tutorial:
  default_locale: zh-cn
  files:
    zh-cn: tutorial/zh-cn.md
  std:
    language: cpp17
    path: tutorial/std.cpp
```

推荐结构：

```text
tutorial/

├── zh-cn.md
└── std.cpp
```

字段说明：

| 字段               | 说明      |
| ---------------- | ------- |
| `default_locale` | 默认题解语言  |
| `files`          | 各语言题解文件 |
| `std.language`   | 标程语言    |
| `std.path`       | 标程路径    |

注意：

```text
tutorial/std.cpp 不用于普通评测
```

它是题目资料，用于题解展示、数据生成或后续验题流程。

---

## 十六、source

`source` 描述题目包来源。

示例：

```yaml
source:
  format: ojos
  fingerprint: ""
```

字段说明：

| 字段            | 说明       |
| ------------- | -------- |
| `format`      | 来源格式     |
| `fingerprint` | 来源指纹或校验值 |

当前 format 可为：

```text
ojos
```

后续可能支持：

```text
polygon
fps
hydro
lemon
unknown
```

该字段用于导入导出和追踪来源。

---

## 十七、A+B Problem 示例

完整目录：

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

`problem.yaml`：

```yaml
schema: ojos.problem.v1
id: 2
slug: 2-a-plus-b
title: A+B Problem
type: traditional
visibility: private
status: draft

limits:
  default:
    time_ms: 1000
    memory_mb: 256
  languages:
    cpp17:
      time_ms: 1000
      memory_mb: 256

statement:
  default_locale: zh-cn
  files:
    zh-cn: statement/zh-cn.md
  assets_dir: statement/assets

runner:
  config: runner/runner.yaml

checker:
  config: checker/checker.yaml

scorer:
  config: scorer/scorer.yaml

tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml

tutorial:
  default_locale: zh-cn
  files:
    zh-cn: tutorial/zh-cn.md
  std:
    language: cpp17
    path: tutorial/std.cpp

source:
  format: ojos
  fingerprint: ""
```

`tests/cases.yaml`：

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

`tests/001.in`：

```text
1 2
```

`tests/001.ans`：

```text
3
```

`checker/checker.yaml`：

```yaml
type: default-trim
name: default-trim-checker
```

`runner/runner.yaml`：

```yaml
type: traditional
name: traditional-runner
```

`scorer/scorer.yaml`：

```yaml
type: default-sum
name: default-sum-scorer
```

---

## 十八、Problem API 与题目包关系

`problem-api` 负责创建和维护题目包。

创建题目时应：

```text
插入 problems 数据库记录
生成 storage/problems/{id}-{slug}
写入 problem.yaml
写入 statement/zh-cn.md
写入 tests/cases.yaml
写入 tests/groups.yaml
写入 runner/runner.yaml
写入 checker/checker.yaml
写入 scorer/scorer.yaml
写入 tutorial/zh-cn.md
写入 tutorial/std.cpp
更新 problems.package_dir
```

更新题目时应同步更新：

```text
数据库元数据
题目包文件
```

删除题目时应：

```text
删除数据库记录
删除或归档题目包目录
```

当前开发阶段可以直接删除题目包目录。

后续生产环境应考虑归档和审计。

---

## 十九、Judge Worker 与题目包关系

Judge Worker 判题时读取：

```text
problems.package_dir
```

然后加载：

```text
problem.yaml
tests/cases.yaml
tests/*.in
tests/*.ans
```

Worker 不读取：

```text
test_cases 数据库表
```

当前流程：

```text
load submission
  ↓
load problem
  ↓
读取 problem.package_dir
  ↓
读取 package_dir/problem.yaml
  ↓
读取 package_dir/tests/cases.yaml
  ↓
按 case.input 读取 input
  ↓
按 case.answer 读取 answer
```

注意：

```text
answer 文件只由 worker 在 jail 外读取
不会复制给用户程序
不会挂载进 nsjail /work
```

---

## 二十、安全边界

题目包目录：

```text
/data/ojos/problems
```

只应对 worker 可见。

用户程序 nsjail 内不应看到：

```text
/data/ojos/problems
```

用户程序只能看到：

```text
/work
```

运行时流程：

```text
worker 读取 tests/001.in
worker 写入 submission cases/001/stdin.txt
worker 不复制 tests/001.ans 到 /work
用户程序在 jail 内读取 /work/stdin.txt
用户程序写 /work/stdout.txt
worker 在 jail 外读取 tests/001.ans
worker 在 jail 外执行 checker
```

这样可以保证：

```text
用户程序不能读取 answer
用户程序不能覆盖原始 input
用户程序不能覆盖 answer
```

---

## 二十一、路径规范

必须严格区分三种路径：

```text
package_dir 相对路径
tests.root 相对路径
case input / answer 相对路径
```

正确示例：

```yaml
tests:
  root: tests
  cases: tests/cases.yaml

cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
```

拼接规则：

```text
cases_yaml = package_dir / tests.cases
input_file = package_dir / tests.root / case.input
answer_file = package_dir / tests.root / case.answer
```

得到：

```text
package_dir/tests/cases.yaml
package_dir/tests/001.in
package_dir/tests/001.ans
```

错误拼接：

```text
package_dir / tests.root / tests.cases
```

会得到：

```text
package_dir/tests/tests/cases.yaml
```

这是错误路径。

---

## 二十二、编码规范

所有文本文件应使用：

```text
UTF-8
```

包括：

```text
problem.yaml
cases.yaml
groups.yaml
statement/*.md
tutorial/*.md
*.in
*.ans
```

对于中文题面，必须确保：

```text
HTTP 请求体使用 UTF-8
服务写文件使用 UTF-8
PowerShell 请求使用 UTF-8 bytes
读取文件时指定 UTF-8
```

PowerShell 提交 JSON 时建议：

```powershell
$json = $obj | ConvertTo-Json -Compress
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)

Invoke-RestMethod `
  -ContentType "application/json; charset=utf-8" `
  -Body $bytes
```

否则中文可能变成乱码。

---

## 二十三、Git 管理规则

不建议提交真实生产题目包：

```text
storage/problems/*
```

可以保留：

```text
storage/problems/.gitkeep
```

`.gitignore` 建议：

```gitignore
storage/problems/*
!storage/problems/.gitkeep
```

原因：

```text
题目数据可能较大
题目数据可能包含未公开比赛题
题目数据可能包含答案
题目包属于运行数据
```

如果需要提交示例题，建议放到单独目录：

```text
examples/problems/
```

而不是直接提交：

```text
storage/problems/
```

---

## 二十四、当前兼容性策略

当前项目仍处于开发阶段。

因此当前策略是：

```text
不兼容旧 test_cases 数据库格式
不兼容旧 no: 0 cases.yaml 格式
不兼容旧 submission_cases 结果格式
不维护旧题目包格式
```

原因：

```text
旧格式还没有稳定发布
开发阶段维护无意义兼容会增加技术债
```

当前应及时删除弃用内容：

```text
test_cases
submission_cases
submissions.code
```

并保持：

```text
problem package
storage/submissions
result.json
```

作为新主线。

未来进入稳定版本后，再考虑：

```text
schema version
migration tool
backward compatibility
import adapter
```

---

## 二十五、常见错误

### 25.1 `load cases.yaml failed`

常见原因：

```text
problem.yaml 中 tests.cases 路径错误
worker 拼接路径错误
cases.yaml 不存在
文件名大小写错误
容器 volume 没挂载 storage
```

重点检查：

```powershell
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\problem.yaml" -Encoding UTF8
Get-Content "D:\Untitled-OJ\storage\problems\2-a-plus-b\tests\cases.yaml" -Encoding UTF8
```

如果错误路径中出现：

```text
tests/tests/cases.yaml
```

说明重复拼接了 `tests.root`。

---

### 25.2 用户程序读到了 answer

这是严重安全问题。

应检查：

```text
nsjail 是否挂载了 /data/ojos/problems
answer 是否被复制到了 case_dir
checker 是否在 jail 内运行并暴露 answer
/work 中是否存在 *.ans
```

正确做法：

```text
只复制 input 到 /work/stdin.txt
不复制 answer 到 /work
checker 在 jail 外读取 answer
```

---

### 25.3 中文题面乱码

常见原因：

```text
请求体不是 UTF-8
PowerShell Body 直接传 string
文件写入编码不一致
读取文件未指定 UTF-8
```

修法：

```powershell
$json = $obj | ConvertTo-Json -Compress
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)

Invoke-RestMethod `
  -ContentType "application/json; charset=utf-8" `
  -Body $bytes
```

---

### 25.4 case_no 从 0 开始

当前不允许。

错误：

```yaml
cases:
  - no: 0
```

正确：

```yaml
cases:
  - case_no: 1
```

原因：

```text
case_no 需要和 cases/001 目录、001.in、001.ans 保持一致
```

---

## 二十六、后续扩展方向

题目包后续需要支持：

```text
多语言题面
题面 assets
样例输入输出
子任务
捆绑点
SPJ
交互器
数据生成器
validator
checker source
runner source
scorer config
提交答案题输出文件
启发式题评分器
导入 Polygon
导出 Polygon
导入 FPS
题目包校验
题目包签名
```

建议扩展方向：

```text
problem.yaml 继续作为入口
tests/cases.yaml 管理测试点
tests/groups.yaml 管理组 / 子任务
checker/ 管 checker
runner/ 管 runner
scorer/ 管 scorer
generators/ 管数据生成器
validators/ 管校验器
```

未来可能结构：

```text
storage/problems/{id}-{slug}/

├── problem.yaml
├── statement/
├── tests/
├── checker/
├── runner/
├── scorer/
├── generators/
├── validators/
├── interactors/
├── attachments/
└── tutorial/
```

---

## 二十七、当前结论

当前 Problem Package 已经完成 OJOS 从：

```text
数据库存测试点
```

到：

```text
文件化题目包
```

的关键升级。

当前核心原则是：

```text
problems 表保存元数据和 package_dir
题面 / 数据 / 配置 / 题解保存在题目包
judge-worker 从题目包读取 problem.yaml 和 cases.yaml
用户程序永远看不到题目包目录
```

这为后续支持：

```text
SPJ
子任务
捆绑点
交互题
通信题
提交答案题
Polygon 导入导出
多赛制评测
```

打下基础。
