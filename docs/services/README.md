# Service 文档

每个正式 Service 在 `services/<name>/` 下提供相邻的 `service.yaml` 和 `release.yaml`：

- `service.yaml` 定义身份、Endpoint 声明、依赖、能力、权限、安全边界和健康检查。
- `release.yaml` 使用 Service Contract v2 定义来源、运行时、`provides/requires` API、events、runtime contract、迁移、路由、资源注册与可观测性。

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
- problem-service 构建确定性内容寻址题包，并通过 transactional outbox 发布 snapshot/tombstone；judge-api 用 inbox/projection 自动同步题目，不允许手工写 Judge 数据库。
- judge-api 接收提交、管理队列和结果状态；Store 部署的 judge-worker 通过 Gateway 长轮询、校验下载源码/题包、在本机沙箱执行并上报结果。Worker 不直连 A 机 PostgreSQL、Redis、MinIO 或 Judge API 私有端口。
- PostgreSQL、Redis、storage-service、MinIO 和 Jaeger 即使由外部系统提供，也要以 Service 和 Endpoint 的形式进入 Topology。
- orchestrator manifest 只声明控制面自身，不扩大 core 对象集合。

Service 不能自行写全局 Topology，也不能绕过 Orchestrator 创建 Link/ApiBinding。secret 只写引用名；生产 workload 身份由 Agent 物化的短期 Deployment JWT 提供，不使用共享 service/worker token。

仓库内含 `local://`、空 image/checksum 的 `release.yaml` 是源码模板，只能用于开发和 Catalog 生成。生产安装必须选择可信 Catalog 中签名、digest-pinned 且平台兼容的 Release；缺少运行 artifact、provider 或 Binding 时 plan 直接失败，不能以元数据登记冒充 installed/running。
