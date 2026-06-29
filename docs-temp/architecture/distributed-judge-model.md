# 分布式评测模型

Judge API 负责提交、队列、Worker endpoint 列表、Worker health/load、任务分发、结果接收和状态更新。

Judge Worker 可部署在 Root 或 Non-root Device 上，暴露独立 Endpoint，连接 Judge API 和 Storage。单机高并发由 Worker 内部配置管理，不依赖同机启动多个 Worker。
