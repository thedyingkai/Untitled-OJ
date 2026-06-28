# Judge Worker Service

Judge Worker 可部署在 Root 或 Non-root Device 上，暴露独立 Endpoint，连接 Judge API 和 Storage。

Worker 内部处理本机并发，配置项包括 `max_parallel_jobs`、`sandbox_slots`、`supported_languages` 和 `resource_profile`。
