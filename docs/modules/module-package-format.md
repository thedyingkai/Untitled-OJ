# Module Package Format

> 文档状态：当前实现，v0 checksum package
> 最后更新：2026-06-27

## Format

`.ojosmod` 是 zip 包，v0 固定结构：

```text
module.yaml
checksums.sha256
README.md              可选
LICENSE                可选
migrations/            可选
assets/                可选
frontend/              v0 只允许声明，不动态执行
services/              v0 只允许声明，不自动执行
```

## Required Verification

`ojosctl module verify <package.ojosmod>` 会检查：

- `module.yaml` 存在并满足 schema_version 1。
- `checksums.sha256` 存在。
- 所有文件 checksum 匹配。
- 所有非 checksum 文件都在 checksum 列表中。
- 无路径穿越。
- 无绝对路径。
- 无 symlink escape。
- 不包含 `.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target`。
- 不包含 hook / script / postinstall / preinstall / 可执行入口。

## Signature Boundary

v0 只做 checksum integrity。以下字段预留给 v1：

```text
signature
signing_key_id
trusted_publisher
```

在 signature / trust policy 完成前，不允许远程不可信模块自动安装。
