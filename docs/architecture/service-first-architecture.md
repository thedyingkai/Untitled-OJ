# Service-first 架构

OJOS 的正式架构是 Installer-first / Service-first / Root Runtime Manager。

Root Installer / Runtime Manager 维护全局状态。Set 描述推荐部署组合，Service 是最小安装和运行单位，Endpoint 使用 `IP:Port` 标识运行实例，Link 描述 Endpoint 到 Endpoint 的连接，Device 描述 Root 与 Non-root 设备，Topology 展示这些对象之间的关系。

旧 Module-first 设计已删除，不再是正式运行模型、契约入口、CLI、API、DB 初始化链路或包格式。
