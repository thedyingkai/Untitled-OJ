# Service 文档

本目录用于保存正式 Service 说明。每个正式 Service 必须提供 `service.yaml` 和相邻的 `release.yaml`：`service.yaml` 定义 Service 身份、Endpoint、requires/provides、权限与运行边界；`release.yaml` 定义发布/导入来源、后端端口和路由发布契约。详细字段见 [Service 规范](../spec/service-spec.md)。

当前基础 Service 包括：

```text
gateway
auth-service
problem-service
user-service
judge-api
judge-worker
postgresql
redis
storage-service
minio
jaeger
orchestrator
```

边界：

- Gateway 是业务流量入口 Service，不是控制面。
- Gateway frontend 是 OJ 站点业务 UI，不是 Orchestrator。
- problem-service 负责题库、题目详情、题目包、数据文件索引和题目权限。
- user-service 负责用户资料、头像、偏好和统计信息。
- judge-api 负责提交、任务队列、worker endpoint 列表、任务分发、结果接收和提交状态更新。
- judge-worker 负责本机编译运行、资源限制和结果上报。
- postgresql、redis、storage-service、minio 和 jaeger 即使由外部系统提供，也必须作为可连接 Service 出现在 Endpoint、Link 和 Topology 中。
- orchestrator 是被声明的控制面 Service，不改变 Orchestrator core 的对象集合。

Service 不得自行修改全局 Topology，也不得绕过 Orchestrator 创建全局 Link。
