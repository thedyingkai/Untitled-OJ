# Service SDK

Service SDK 只提供 Service 契约、目录模板和校验样例。它不作为产品入口，不执行仓库脚本，不写入明文 secret，也不生成 `privileged`、`cap_add` 或 host mount 配置。

正式入口是 OJOS Orchestrator GUI / TUI。开发者可以参考 `sdk/templates/service.yaml` 编写 `service.yaml`，再由 Orchestrator 导入、校验、生成计划并执行受控操作。

Service 模板必须遵守：

- `service.yaml` 是唯一正式契约。
- Endpoint 只声明 `default_port`，实际 `IP:Port` 由 Orchestrator 绑定。
- Link 只声明需求，真实连接由 Orchestrator 创建。
- secret 只能使用 `secret_ref` 或 `security.required_secrets` 表达。
- 不允许任意 command、脚本、hook、`privileged`、`cap_add` 或 host mount。
