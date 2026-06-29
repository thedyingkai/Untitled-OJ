# Non-root Agent

Non-root Agent 负责加入 Root、接收 Service 安装计划、下载 Service、启动 Service、停止 Service、暴露 Endpoint、上报 Health、上报 Logs、上报 Endpoint 可达性并接收 Link 配置。

Agent 不能自行配置全局 Link，不能热插拔全局 Service，不能选择 Set，不能运行 Web Shell 或 Root Installer GUI。
