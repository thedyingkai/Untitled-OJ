# Module Manifest

> 文档状态：当前实现，`schema_version: 1`
> 最后更新：2026-06-28

OJOS 模块通过 `modules/<module>/module.yaml` 声明。Manifest 是模块 package 与 Kernel 之间的契约，只描述能力，不保存 secret，也不包含可执行 hook。

## 基础结构

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
    - id: ojos.platform.web-shell
      version: ">=0.1.0"

provides:
  permissions: []
  roles: []
  components: []
  services: []
  workers: []
  frontend_routes: []
  menus: []
  gateway_routes: []
  storage_buckets: []
  health_checks: []
  migrations: []
  events:
    publishes: []
    subscribes: []
  scheduled_jobs: []
  admin_panels: []
  topology:
    nodes: []
    edges: []
```

## 校验规则

Installer Core 校验：

- `id` 符合 `[a-z0-9][a-z0-9.-]*`。
- `version` 是 semver。
- `schema_version` 受支持。
- `kind` 是 `kernel`、`platform`、`feature`、`integration` 或 `metadata`。
- `status` 是 `builtin`、`external` 或 `demo`。
- permission、role、component、service、worker、route、menu、gateway prefix、bucket、health check、job、admin panel、topology node、dependency 不重复。
- dependency 不能 self reference。
- migration 必须是相对 `deploy/migrations/*.sql` 路径，且 `up/down` 成对。

## 禁止字段

Manifest 不能包含以下字段名：

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
target_url
image
mount
host_path
privileged
cap_add
```

v0.1.0 不执行 hook，不下载 remote module，不加载 dynamic frontend bundle。

## 路径安全

`validate_manifest_file` 要求：

- manifest path 为相对路径。
- manifest 位于 repo root 的 `modules/` 下。
- canonical path 仍在 `modules/` 内。
- 文件名为 `module.yaml`。
- 禁止 `..`、absolute path、symlink escape、`.tmp`、`.env`、`node_modules`、`frontend/dist`、`target`、`.git`。

## Signature 字段

以下字段为 package trust policy 预留：

```yaml
signature:
signing_key_id:
trusted_publisher:
```

v0.1.0 只验证本地 package checksum integrity，不自动安装远程不可信模块。
