# Module Package Format

> 文档状态：当前实现，package format v1

`.ojosmod` 是 zip package。Package format `1` 必须包含：

```text
module.yaml
checksums.sha256
package.yaml
```

可选 metadata-only 内容：

```text
README.md
LICENSE
migrations/
assets/
frontend/
services/
tests/
```

v1 中 `frontend/` 和 `services/` 只作为元数据或源码材料保存。OJOS 不会从 package 动态执行前端 bundle、service command、hook 或 script。

## package.yaml

```yaml
package:
  format: ojosmod
  version: 1
  created_by: ojosctl
  signature: null
  signing_key_id: null
  trusted_publisher: null
```

## 校验规则

`ojosctl module verify <package.ojosmod>` 检查：

- `module.yaml` 存在并符合 `schema_version: 1`。
- `checksums.sha256` 存在。
- `package.yaml` 存在，且 `package.format=ojosmod`、`package.version=1`。
- 每个文件 checksum 匹配。
- 每个非 checksum 文件都在 checksum manifest 中。
- 拒绝 path traversal。
- 拒绝 absolute path。
- 拒绝 symlink。
- 拒绝 `.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target`。
- Manifest 和 package validation 拒绝 hook/script/postinstall/preinstall/executable 语义。

## Signature 边界

Package v1 只验证 checksum integrity，不证明 publisher trust。

以下字段为保留字段，可为 `null`：

```text
signature
signing_key_id
trusted_publisher
```

Remote module market 在 package signature 和 publisher trust policy 完成前不支持。

## 版本冻结

`.ojosmod` package format 当前为 `1`。破坏性 package layout 变更必须进入 package format `2`。
