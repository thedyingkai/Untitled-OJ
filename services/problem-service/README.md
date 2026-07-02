# Problem Service

`problem-service` owns problem metadata and the canonical problem package format.

## Problem Package Format

Each problem is stored as one directory under `Storage.ProblemsRoot`:

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
  runner/runner.yaml
  checker/checker.yaml
  validators/validator.yaml
  scorer/scorer.yaml
  attachments/
  validators/
  generators/
```

`problem.yaml` is the package manifest. The current schema is `ojos.problem.v1`.

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

Problem statements and tutorials are raw Markdown files with embedded LaTeX support. Inline math uses `$...$`, block math uses `$$...$$`; rendering is a frontend concern.

Test cases are indexed in `tests/cases.yaml`:

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

Case-level limits are optional. When absent, the judge should use the problem default limit or the language override from `problem.yaml`.

## Problem Types And Components

Supported problem types in the package manifest:

```text
traditional
interactive
communication
output_only
heuristic
```

Each problem has four component slots:

```text
runner
checker
validator
scorer
```

Every component is configured by a YAML file with this shape:

```yaml
type: builtin
name: default-trim-checker
config:
  trim_trailing_spaces: true
  ignore_trailing_blank_lines: true
```

Custom components use `type: custom` and must point at a source file stored inside the problem package:

```yaml
type: custom
name: special-checker
config:
  language: cpp17
  source: checker/special_checker.cpp
  args:
    - --strict
```

The API accepts `runner`, `checker`, `validator`, and `scorer` objects with `type`, `name`, `language`, `source_path`, `source_code`, and `args`. When `source_code` is provided, `problem-service` writes it into `source_path`; if `source_path` is omitted, a default path under the component directory is selected.

Current storage/validation supports custom components. Judge execution still needs runner dispatch support before custom components can be executed in the live judge path.
