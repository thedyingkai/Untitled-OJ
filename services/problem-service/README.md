# Problem Service

`problem-service` 管题目元数据、authoring 工作区和不可变题目 artifact。生产环境的 `/data/ojos/problems` 由签名的 `standard-container-v1` RETAIN managed volume 提供，仅挂载给 Problem Service，用于 live/staging/backup/journal 和确定性 package build；它不是 Problem 与 Judge 的共享目录。发布后的题包和文件通过 Service Contract v3 声明的 Storage API Binding 写入内容寻址对象存储。

## 生产 artifact 与 Judge 投影

每次在线创建、更新测试数据或删除题目都遵循同一条闭环：

1. 在隔离 staging 目录构建并校验题目树；确定性 ZIP 对相同输入产生完全相同的字节与 SHA-256。
2. 通过 `storage.object.put` requirement 的热重载 Binding 条件写入 `problems/package-sha256-<digest>.zip`；authoring 文件使用 `problem-<id>-objects-sha256-<digest>`。上传前先登记持久 intent，响应必须与预期 SHA-256/size 一致。
3. 在一个 PostgreSQL 事务中更新 Problem、递增 `aggregate_version`/必要时的 `package_revision`、保存不可变 revision，并写入 `integration_outbox`。事务失败不会发布 snapshot；未关联上传由 GC ledger 回收。
4. relay 将 `io.ojos.problem.snapshot.v1` 或 `io.ojos.problem.deleted.v1` CloudEvent 投递到 Redis Stream。Judge API 通过 consumer group、inbox 和幂等 projection 消费，只有更大的 aggregate version 能推进投影。
5. Submission 创建时复制题包 revision、artifact URI、SHA-256 和 size。题目后续更新或删除不会改变已经创建的任务；删除后禁止新提交，旧任务仍可完成。

生产路径不执行 Judge 数据库手工 `INSERT`，也不把 package 目录挂载给 Judge/Worker。旧数据使用可断点 backfill/reconcile，通过相同 snapshot 契约补齐。

对象 GC 的 ownership、retention、最终引用检查和 `NEEDS_ATTENTION` 操作见 [Problem artifact lifecycle](ARTIFACT_LIFECYCLE.md)。

## 题目包目录

每道题对应一个 `<problem-id>-<slug>` 目录：

```text
<problem-id>-<slug>/
  problem.yaml
  statement/
    zh-cn.md
    assets/
  tutorial/
    zh-cn.md
    std.cpp
  tests/
    groups.yaml
    cases.yaml
    001.in
    001.ans
  runner/
    runner.yaml
  checker/
    checker.yaml
  validators/
    validator.yaml
  scorer/
    scorer.yaml
  attachments/
  generators/
```

所有 manifest 路径都必须留在题目包内。绝对路径和含 `..` 的父目录跳转会被拒绝。

## `problem.yaml`

当前 schema 是 `ojos.problem.v1`：

```yaml
schema: ojos.problem.v1
id: 1001
problem_no: P1001
slug: 1001-a-plus-b
title: A+B
type: traditional
visibility: public
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
      time_ms: 3000
      memory_mb: 512

statement:
  default_locale: zh-cn
  format: markdown+latex
  files:
    zh-cn: statement/zh-cn.md
  assets_dir: statement/assets

tutorial:
  default_locale: zh-cn
  format: markdown+latex
  files:
    zh-cn: tutorial/zh-cn.md
  std:
    language: cpp17
    path: tutorial/std.cpp

tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml

runner:
  config: runner/runner.yaml
checker:
  config: checker/checker.yaml
validator:
  config: validators/validator.yaml
scorer:
  config: scorer/scorer.yaml

source:
  format: ojos
  fingerprint: ""
```

字段限制：

- `type` 可取 `traditional`、`interactive`、`communication`、`output_only`、`heuristic`。
- `visibility` 可取 `private`、`public`。
- `status` 可取 `draft`、`ready`、`published`、`archived`。
- 时间限制范围是 1 到 600000 ms，内存限制范围是 1 到 65536 MiB。
- 题面和题解只接受 `markdown+latex`。行内公式用 `$...$`，块级公式用 `$$...$$`；渲染由业务前端完成。

## 测试数据

`tests/cases.yaml` 索引输入和答案文件。`input`、`answer` 相对于 `tests.root`：

```yaml
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: 100
    group: 0
    sample: true
    hidden: false
    time_limit_ms: 1000
    memory_limit_mb: 256
```

`case_no` 必须为正整数且不能重复。case 级时间、内存可以省略；省略后使用语言覆盖，再回退到题目默认值。

`tests/groups.yaml` 保存分组计分元数据：

```yaml
groups:
  - group_no: 0
    name: default
    score: 100
    rule: sum
    feedback: full
```

`rule` 可取 `sum`、`min`、`max`、`any`、`all_or_nothing`。省略 `tests.groups` 或提供空列表时只隐式声明默认组 `0`；case 省略 `group` 时也默认为 `0`。引用其他未声明 group 的题包无效，Problem 在发布前拒绝，Worker 也会拒绝加载。

## 判题组件

每个题目包有 runner、checker、validator、scorer 四个插槽。内置组件配置示例：

```yaml
type: builtin
name: default-trim-checker
config:
  trim_trailing_spaces: true
  ignore_trailing_blank_lines: true
```

不同题型的默认 runner/checker：

| 题型 | Runner | Checker | Scorer |
| --- | --- | --- | --- |
| `traditional` | `traditional-runner` | `default-trim-checker` | `default-sum-scorer` |
| `interactive` | `interactive-runner` | `interactive-checker` | `default-sum-scorer` |
| `communication` | `communication-runner` | `communication-checker` | `default-sum-scorer` |
| `output_only` | `output-only-runner` | `output-only-checker` | `default-sum-scorer` |
| `heuristic` | `heuristic-runner` | `heuristic-checker` | `heuristic-scorer` |

默认 validator 是 `default-input-validator`。

自定义组件使用 `type: custom`，源码必须放在题目包内：

```yaml
type: custom
name: special-checker
config:
  language: cpp17
  source: checker/special_checker.cpp
  args:
    - --strict
```

创建和更新 API 接受 `runner`、`checker`、`validator`、`scorer` 对象，字段为 `type`、`name`、`language`、`source_path`、`source_code`、`args`。提供 `source_code` 后，Problem Service 会写入 `source_path`；没有给路径时，按组件类型和语言选择包内默认路径。

judge-worker 已支持编译和执行四类 custom component。组件语言必须存在于 worker 的语言配置，源码和执行过程都走 worker 沙箱。Problem Service 的 package validation 只证明目录、manifest 和引用关系合法，不替代 worker 侧的编译与协议测试。

## Service provider 路由

Service Contract v3 的 provider path 不带 `/problem` 前缀；Gateway 根据 deployment-scoped exposure mount 编译公开路径。旧 `/problem/**` 路由仅作为一个迁移版本的兼容别名保留。

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/healthz`、`/readyz` | 进程健康；数据库、required Binding、事件 transport 与 authoring volume 就绪 |
| `POST` / `GET` | `/problems` | 创建、分页查询题目 |
| `GET` / `PUT` / `DELETE` | `/problems/{id}` | 读取、更新、删除题目 |
| `GET` | `/problems/{id}/package` | 读取包摘要和校验结果 |
| `POST` | `/problems/{id}/package/validate` | 重新校验题目包 |
| `GET` | `/problems/{id}/package/cases` | 查看包内 case |
| `POST` / `GET` | `/problems/{id}/test-cases` | 新增、查询测试点 |
| `PUT` / `DELETE` | `/problems/{id}/test-cases/{case_no}` | 更新、删除测试点 |

外部访问路径由当前 active Contribution revision 决定，不能把 provider path 或兼容别名直接当成公开 API 地址。
