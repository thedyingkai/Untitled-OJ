# Service SDK

这个目录提供 Service 契约说明、模板和校验参考，不是产品入口，也不会执行仓库脚本。

开发者从 `sdk/templates/service.yaml` 复制身份契约，再为同一版本编写相邻的 `release.yaml`。Orchestrator Web UI 或 TUI 负责导入、校验、生成计划和执行受控动作。

两份契约各有职责：

- `service.yaml` 描述 Service 身份、依赖、能力、Endpoint 声明、权限和安全边界。
- `release.yaml` 描述来源、运行时、迁移、route、API surface 和资源注册。

基本规则：

- Service ID、SemVer、类型、后端协议、端口和健康路径必须在两份文件中一致。
- Endpoint 只声明默认端口；运行时身份由 Orchestrator 绑定为 `ip:port:service-name`。
- Link 只声明依赖，真实连接由 Orchestrator 创建。
- secret 只写引用名，不能把值放进 manifest。
- `service.yaml` 禁止任意 command、脚本、hook、`privileged`、`cap_add` 和 host mount。
- local-process 的固定 command 只允许写在 `release.yaml`，并受路径和字符校验。

完整字段和交叉校验见 [Service 规范](../../docs/spec/service-spec.md)。
