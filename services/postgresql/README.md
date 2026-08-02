# PostgreSQL Service

这个目录只定义 PostgreSQL 的 Service/Release 契约。`runtime.mode: external` 表示 Orchestrator 不负责启动数据库，
并不表示数据库已经运行。登记现有实例时要在安装请求中传 `external_service_running=true`；该选项与
`execute_service_driver=true` 互斥，Endpoint 必须通过健康检查，也不能覆盖仍可能活动的旧部署。登记成功后，
Orchestrator 才会保存 `postgres` Endpoint、创建 Link 并把它放进 Topology，运行时所有者记为 `external`。

默认端口是 5432，当前健康检查只验证 TCP 连接。数据库名和 `postgres-password` 是配置/secret 引用，真实凭据不能写进 manifest。

业务 schema 的 migration 由各消费 Service 的 `release.yaml` 声明，不由 PostgreSQL Service 统一打包。
