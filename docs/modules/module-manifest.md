# Module Manifest

> 文档状态：当前实现，schema_version 1
> 最后更新：2026-06-27

## 基本结构

模块 manifest 必须使用 `schema_version: 1`：

```yaml
schema_version: 1
id: ojos.demo-module
name: Demo Module
version: 0.1.0
set: demo
kind: feature
status: demo
description: Installer validation demo module.

compatibility:
  platform: ">=0.1.0"
  installer: ">=0.1.0"

requires:
  modules:
    - id: ojos.kernel.edge-ui-shell
      version: ">=0.1.0"

provides:
  permissions:
    - key: demo.view
      description: View demo module metadata.
  components:
    - id: demo-component
      type: metadata
      status: DISABLED
      config: {}
  frontend_routes: []
  menus: []
  gateway_routes: []
  storage:
    buckets: []
  health_checks: []
  migrations: []
```

## 校验规则

Rust installer core 会校验：

- `id` 只能匹配 `[a-z0-9][a-z0-9.-]*`。
- `version` 必须是 semver。
- `schema_version` 必须是支持版本。
- `kind` 必须是 `kernel`、`feature`、`integration` 或 `metadata`。
- `status` 必须是 `builtin`、`external` 或 `demo`。
- permission key、component id、route path、menu key、gateway prefix、bucket、health check id 不得重复。
- dependency 不得重复，不得 self dependency。
- migration 只能声明 `deploy/migrations/*.sql`，且 up/down 成对。

## 危险字段

manifest 不允许包含以下字段名：

```text
secret
token
password
private_key
env
command
script
hook
postinstall
preinstall
remote_url
download_url
```

v0 不执行任何脚本，也不支持远程下载模块。

## 路径安全

`validate_manifest_file` 要求 manifest 路径：

- 是相对路径。
- 位于 repo root 的 `modules/` 下。
- canonicalize 后仍在 `modules/` 下。
- 文件名必须是 `module.yaml`。
- 不允许 `..`、绝对路径、symlink escape、`.tmp`、`.env`、`node_modules`、`frontend/dist`、`target`、`.git`。

## 签名字段

保留字段：

```yaml
signature:
signing_key_id:
trusted_publisher:
```

v0 只做 checksum integrity。signature / trust policy 留到 v1，因此 v0 不应安装远程不可信模块。
