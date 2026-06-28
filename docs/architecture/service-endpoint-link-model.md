# Service / Endpoint / Link 模型

Service 是最小可安装、可运行、可启停、可热插拔、可暴露端口、可连接的功能单元。

Endpoint 的主标识是 `IP:Port`。Service 只能声明 `default_port`，实际 IP 和端口由 Root Runtime Manager 配置。

Link 的主标识是 `source endpoint -> target endpoint`。Link 可附带 `protocol`、`auth_mode`、`scope`、`health`、`latency`、`config_ref` 和 `secret_ref`。
