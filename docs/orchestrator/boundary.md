# Orchestrator v1 边界

OJOS Orchestrator 只负责控制面编排，不承载或代理 OJ 业务请求。Gateway 是受管服务，负责业务流量、认证中间件和路由；Orchestrator 通过受控管理接口发布配置，但不进入业务数据面。

## 正式进程

- `ojos-orchestrator-daemon`：远程单主动控制面。生产模式必须使用 PostgreSQL、TLS、OIDC、Node CA 和可信 Catalog；缺少任一项时在绑定端口前失败。只有显式 `--ephemeral` 才允许内存模式。
- `ojos-orchestrator-desktop`：本地宿主。默认在应用数据目录创建 SQLite，并在同一进程启动 loopback backend 和 loopback Agent；原生 WebView 加载同源 Web UI，不打开外部浏览器。
- `ojos-orchestrator-agent`：Node worker。只领取分配给本 Node 的持久 Job，通过 Docker Engine API 执行固定类型任务，不接受任意 shell。
- `ojos-orchestrator-tui`：远程控制客户端，使用与 Web 相同的 `/api/v1` 契约。

Desktop 外部连接模式只连接用户明确指定的控制面。它允许控制面 origin，以及从 `/api/v1/auth/config` 动态发现且精确匹配的 HTTPS OIDC authorization origin；其他导航和新窗口均拒绝。

## 模块职责

- `orchestrator-core`：领域模型、校验、确定性 diff/plan、状态机和 published action 定义。
- `orchestrator-legacy`：隔离 0.2 Console、旧 PostgreSQL 仓储、仓库文件加载、HTTP/TCP 探测、归档解包和本地进程/Docker Compose 适配；v1 代码不得把这些实现放回 `orchestrator-core`。
- `orchestrator-storage`：Memory/SQLite/PostgreSQL 仓储、迁移、事务、数据库锁和 readiness。
- `orchestrator-control-plane`：Operation、Job、lease、恢复、补偿与 reconciler 协调。
- `orchestrator-runtime`：Docker Engine API 与固定运行时契约。
- `orchestrator-manager`：Catalog/Release/Store 用例编排。
- `orchestrator-agent`：Node 本地执行账本、运行时执行和类型化 provider。

持久数据库事务只覆盖状态转换；Catalog 下载、Docker、健康探测和 provider I/O 均在事务外执行。生产持久路径没有全表内存镜像，也没有进程级 console mutex。

## 数据所有权

- Store 拥有 Catalog、Release、Deployment、RuntimeInstance 和安装位置。
- Topology 只拥有已经注册或部署服务之间的期望 Endpoint/Link，以及不可变 Revision。
- TopologyStatus 拥有真实健康、观测 revision、drift 和最后 Operation。
- 每用户画布布局属于 UI state，不属于 TopologySpec。
- Operation、Job、attempt、event、审计和诊断各自持久化，不写入 TopologySpec。

旧 `topology_snapshots` 在升级时只导入为未应用 draft；旧 HostService 只能生成 `External/Unknown` 投影，不伪造容器 ID 或 RepoDigest。

## 明确不做

v1 不提供 active-active、多租户计费、通用调度器、自动扩缩容、Kubernetes runtime 或任意本地命令执行。服务节点由安装请求显式选择；Topology 不会隐式安装服务。
