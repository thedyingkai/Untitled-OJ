# Service 文档

每个正式 Service 在 `services/<name>/` 下提供相邻的 `service.yaml` 和 `release.yaml`：

- `service.yaml` 定义身份、Endpoint 声明、依赖、能力、权限、安全边界和健康检查。
- `release.yaml` 定义来源、运行时、迁移、路由、API surface、资源注册与可观测性。

两份文件必须使用相同的 Service ID、SemVer、类型、后端协议、端口和健康路径。详细字段见 [Service 规范](../spec/service-spec.md)。

仓库当前有 12 个基础 Service：

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

职责边界：

- Gateway 是业务流量入口，不是控制面。
- `services/gateway/frontend` 是 OJ 业务 UI；`manager/web` 才是 Orchestrator Web UI。
- problem-service 管题库、题目详情、题目包、数据文件索引和题目权限。
- user-service 管用户资料、头像、偏好和统计信息。
- judge-api 接收提交、管理队列和结果状态；judge-worker 在本机编译运行并上报结果。
- PostgreSQL、Redis、storage-service、MinIO 和 Jaeger 即使由外部系统提供，也要以 Service 和 Endpoint 的形式进入 Topology。
- orchestrator manifest 只声明控制面自身，不扩大 core 对象集合。

Service 不能自行写全局 Topology，也不能绕过 Orchestrator 创建 Link。secret 只写引用名，实际值由部署环境提供。

仓库内的 Service 包主要是契约包。若 `release.yaml` 没有可用的 image/binary，或 local-process 仍指向源码工作目录，安装只能完成目录、计划和元数据注册；真正启动还需要相应运行资产。
