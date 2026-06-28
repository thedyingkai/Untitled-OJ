# service.yaml 契约

`service.yaml` 是 OJOS 正式 Service 契约。旧 `module.yaml` 只保留 legacy compatibility。

必需字段：`id`、`name`、`version`、`kind`、`endpoint`、`runtime`、`requires`、`provides`、`ui`、`permissions`、`security`。

安全规则：

- 不允许任意脚本、任意 command、hook、postinstall 或 preinstall。
- 不允许 privileged、cap_add 或 host mount。
- Secret 只能使用 `secret_ref`，不能写明文。
- Service 只能声明 `default_port`，实际 Endpoint 由 Root 配置。
- Service 只能声明 required/optional links，实际 Link 由 Root 配置。
