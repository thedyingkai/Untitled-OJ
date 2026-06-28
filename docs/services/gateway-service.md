# Gateway Service

Gateway 是普通 Service，负责外部 HTTP 入口、鉴权、权限校验、路由转发、统一错误、审计和基础限流。

Gateway 不运行 Root 控制面，不决定全局拓扑，不安装或热插拔 Service。
