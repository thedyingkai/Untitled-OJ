# Service-first 架构

OJOS 的正式架构是 Installer-first / Service-first / Root Runtime Manager。

Root Installer / Runtime Manager 维护全局状态，Set 描述推荐组合，Service 是最小安装和运行单位，Endpoint 使用 `IP:Port` 标识运行实例，Link 描述 Endpoint 到 Endpoint 的连接，Device 描述 Root 与 Non-root 设备，Topology 展示这些对象之间的关系。

Module 只作为 legacy compatibility 存在，不再是一等对象，不再作为正式契约入口。
