# Service 文档

本目录用于保存正式 Service 说明。Service 的唯一正式契约是 `service.yaml`，详细字段见 [Service 规范](../spec/service-spec.md)。

当前基础 Service 包括：

```text
gateway
web-shell
auth
problem-api
judge-api
judge-worker
postgres
redis
storage
```

边界：

- Gateway 是业务流量入口 Service，不是控制面。
- Web Shell 是 OJ 站点业务 UI Service，不是 Orchestrator。
- problem-api 负责题库、题目详情、题目包、数据文件索引和题目权限。
- judge-api 负责提交、任务队列、worker endpoint 列表、任务分发、结果接收和提交状态更新。
- judge-worker 负责本机编译运行、资源限制和结果上报。
- postgres、redis 和 storage 即使由外部系统提供，也必须作为可连接 Service 出现在 Endpoint、Link 和 Topology 中。

Service 不得自行修改全局 Topology，也不得绕过 Orchestrator 创建全局 Link。
