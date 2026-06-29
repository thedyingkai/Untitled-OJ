# Device 模型

Root Device 运行 Root Installer / Runtime Manager，拥有全局 Service、Endpoint、Link、Set、Topology 和 Device 配置权。

Non-root Device 只运行 node-agent 和后端 Service。它不能配置全局 Link，不能热插拔全局 Service，不能运行 Web Shell，也不能运行 Root Installer GUI。
