# Non-root Device Agent

Non-root Device Agent 负责加入 Root、接收安装计划、下载 Service、启停 Service、暴露 Endpoint、上报 Health/Logs/Reachability 并接收 Link 配置。

Agent 不配置全局 Link，不热插拔全局 Service，不选择 Set，不运行 Web Shell，也不运行 Root Installer GUI。
