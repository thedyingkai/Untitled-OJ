> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# OJOS Problem Package Format

This is the canonical OJOS problem package format. Legacy database-backed
`test_cases` formats and `case_no: 0` packages are not accepted.

## Directory Layout

```text
storage/problems/{id}-{slug}/
  problem.yaml
  statement/
    zh-cn.md
    assets/
  tests/
    cases.yaml
    groups.yaml
    001.in
    001.ans
  checker/
    checker.yaml
  runner/
    runner.yaml
  scorer/
    scorer.yaml
  tutorial/
    zh-cn.md
    std.cpp
```

All paths stored in YAML are relative logical paths. Public APIs may return
logical paths such as `tests/cases.yaml` or `001.in`, but must never return a
server absolute path.

## problem.yaml

Required at package root.

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
    c11:
      time_ms: 1000
      memory_mb: 256
    python3:
      time_ms: 3000
      memory_mb: 512
    java17:
      time_ms: 2000
      memory_mb: 512

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

Rules:

- `schema` must be `ojos.problem.v1`.
- `id` must be positive.
- `slug` and `title` must be non-empty.
- `type` must be one of `traditional`, `interactive`, `communication`, `output_only`, `heuristic`.
- `visibility` must be one of `private`, `public`, `contest_only`.
- `status` must be one of `draft`, `ready`, `published`, `archived`.
- `limits.default.time_ms` must be `1..600000`.
- `limits.default.memory_mb` must be `1..65536`.
- Language limits use the same ranges.

## tests/cases.yaml

Required at the path referenced by `problem.yaml: tests.cases`.

```yaml
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: 100
    group: 0
    sample: false
    hidden: true
    time_limit_ms: 1000
    memory_limit_mb: 256
```

Path rules:

- `tests.root`, `tests.groups`, and `tests.cases` are relative to package root.
- `case.input` and `case.answer` are relative to `tests.root`.
- Do not join `tests.root + tests.cases`; that produces `tests/tests/cases.yaml`.
- Absolute paths and `..` path segments are invalid.

Case rules:

- `case_no` must be positive and unique.
- `input` must exist.
- `answer` must exist.
- `score` must be non-negative.
- Optional `time_limit_ms` and `memory_limit_mb` use the same ranges as default limits.
- Sample cases may be shown on the public problem detail page; hidden answers are never exposed.

## tests/groups.yaml

Recommended for future subtask support.

```yaml
groups:
  - group_no: 0
    name: default
    score: 100
    rule: sum
    feedback: full
```

Group rules:

- `group_no` must be non-negative and unique.
- `score` must be non-negative.
- `rule` may be empty or one of `sum`, `min`, `max`, `any`, `all_or_nothing`.

## Components

Current built-in component configs:

```yaml
# runner/runner.yaml
type: builtin
name: traditional-runner
config: {}
```

```yaml
# checker/checker.yaml
type: builtin
name: default-trim-checker
config:
  trim_trailing_spaces: true
  ignore_trailing_blank_lines: true
```

```yaml
# scorer/scorer.yaml
type: builtin
name: default-sum-scorer
config: {}
```

Rules:

- Component config path must be present in `problem.yaml`.
- Component YAML must parse.
- `type` must be `builtin` or `custom`.
- `name` must be non-empty.
- Built-in names must be supported by the current worker.

## Validator

Problem API exposes package management endpoints:

```text
GET  /api/problem/problems/{id}/package
POST /api/problem/problems/{id}/package/validate
GET  /api/problem/problems/{id}/package/cases
```

All three require `problem.manage.data` on the problem scope or an equivalent
admin role. Responses return only logical paths and validation summaries.

Validator checks:

- `problem.yaml` exists and is valid YAML.
- `tests/cases.yaml` exists and is valid YAML.
- Case numbers are positive and unique.
- Input and answer files exist.
- Scores and resource limits are valid.
- Component configs are valid.
- YAML files are at most 1 MiB.
- Individual package files are at most 64 MiB.
- Total package size is at most 512 MiB.
- Symlinks are rejected.
- Absolute paths and parent path traversal are rejected.
- Legacy formats are rejected.

## Acceptance Commands

With a valid admin token:

```powershell
$token = "<admin-token>"
$headers = @{ Authorization = "Bearer $token" }

Invoke-RestMethod `
  -Headers $headers `
  -Uri "http://localhost:8080/api/problem/problems/2/package"

Invoke-RestMethod `
  -Method Post `
  -Headers $headers `
  -Uri "http://localhost:8080/api/problem/problems/2/package/validate"

Invoke-RestMethod `
  -Headers $headers `
  -Uri "http://localhost:8080/api/problem/problems/2/package/cases"
```

Expected result for a normal A+B package:

- `validation.valid` is `true`.
- `package.total_cases` is greater than `0`.
- `package.checker.name` is `default-trim-checker`.
- `package.scorer.name` is `default-sum-scorer`.
- `package.runner.name` is `traditional-runner`.

Negative checks:

- Remove `tests/001.in`: validator returns `missing_input`.
- Remove `tests/001.ans`: validator returns `missing_answer`.
- Set `score: -1`: validator returns `invalid_score`.
- Break YAML indentation: validator returns `invalid_yaml`.
- Set `input: ../secret.txt`: validator returns `path_escape`.
