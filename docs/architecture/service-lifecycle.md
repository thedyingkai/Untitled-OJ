# Go 服务生命周期

Auth、Gateway、Judge API、Problem、Storage 和 User 的启动入口统一按职责分层，但继续使用既有 GoZero HTTP 运行方式。Contest 参考服务仍使用共享 `bootstrap`；本次没有把两种宿主强行合并。

## 修改入口

| 位置 | 负责什么 | 不负责什么 |
| --- | --- | --- |
| 服务根目录的 `main` | 产品健康检查命令、配置文件参数、配置加载、进程退出码 | 不创建数据库连接、不注册路由、不管理后台任务 |
| `internal/app/app.go` | 组装服务上下文、HTTP server、中间件和路由；登记退出清理 | 不包含业务规则，不在组装失败时直接退出进程 |
| `internal/svc/startup.go` | 按服务依赖构造资源；失败时释放已经取得的资源 | 不调用 `log.Fatal` 或 `os.Exit` |
| `internal/svc/servicecontext.go` 或 `service_context.go` | 已初始化依赖的所有者、业务依赖访问入口和 `Close` | 不同时承担环境解析、探针和全部后台循环 |
| `internal/svc/environment.go` | 既有环境覆盖、托管模式约束和配置校验 | 不启动服务 |
| `internal/svc/health.go` | 服务特有的 readiness 条件 | 不以构造成功永久代替依赖健康 |
| Judge/Problem 的 `background.go` | 事件消费者、outbox relay、投影与题包清理的启动和取消范围 | 不终止进程、不拥有第二套业务状态 |
| Gateway 的 `contribution.go` | Contribution 快照、路由投影、确认与后台同步 | 不承担 HTTP 进程入口 |

Storage 的 workload 公钥读取与校验位于 `workload_identity.go`。其余领域服务、仓储、HTTP handler 和生成契约保持原归属，不因生命周期拆分而搬入 `app`。

## 资源所有权与失败回滚

每个连接池、Redis 客户端、ContextProvider、代理或 tracing provider 在创建后立即归属当前服务上下文。构造返回错误时，用同一 `Close` 路径释放部分已初始化状态；正常构造成功后，所有权交给 `app.Run`。Auth 保留已有的具名返回值回滚和管理员初始化密钥清零。

Gateway、Judge、Problem 的构造函数现在返回 `(*ServiceContext, error)`。生产调用方已经由 `internal/app` 承接；不要在 handler 或其他包中重新引入不可回收的进程退出。Storage 的 `BuildServiceContext` 是生产组装入口，历史 `NewServiceContext` 包装仍保留。

Judge 在启动后台 goroutine 前完成消费者配置。Problem 将 relay 和题包 GC 配置失败向上返回；如果先前已经启动投影或 relay，失败回滚会取消并等待它们，再关闭其依赖。开发环境中显式关闭 GC 的原行为不变。

`app.Run` 在 HTTP 配置前登记上下文清理，使用返回错误的 `rest.NewServer`。退出清理先执行 HTTP 层既有的 `Stop`，再释放服务依赖；后台任务取消与等待仍由各服务的 `Close` 负责。Judge 的 Prometheus collector 注册失败返回错误，成功注册的 collector 随应用退出注销。

## 保留的运行语义

- GoZero 继续拥有进程信号处理和 HTTP drain。`Server.Stop` 本身不等于主动取消 `Server.Start`；不能把本次拆分解释为新增了由调用方 Context 控制的 HTTP 生命周期。
- `Close` 中传入的 5 秒上下文约束接受该上下文的清理操作，不承诺所有第三方关闭函数都有硬性总超时。
- 原来的健康检查命令、端口、路由、中间件和 readiness 条件保留。Gateway 缺失有效 Contribution 投影时仍不 ready；Judge 的 Redis 启动失败仍按既有约定降级到 PostgreSQL 任务真值，并由后台重试。
- 本次没有更改 API、数据库 schema、JWT、权限或事件格式，也没有移除 Agent、Docker 或题包验证能力。

## 本批验证范围

当前源码的仓库外副本已通过 6 个服务的 424 项顶层 Go 回归，其中包括 6 项新增的启动失败资源回收检查。真实 TLS PostgreSQL 和带认证 Redis 用于连接回收、Auth 持久化、Judge 任务与事件投影等场景；85 个声明的职责拆分和健康检查入口另做了保真核对。

I 批在 Windows 跳过的题包符号链接拒绝、MinIO 对象生命周期两项，已在 J 批分别使用真实 Linux 文件系统和隔离 MinIO 补跑通过。J 批还实际启动了当前源码构建的 Gateway、Auth 和生产模式 daemon，验证登录/权限链路与 OIDC，并确认三个进程收到 SIGTERM 后正常退出、PostgreSQL 会话释放。其余四个 GoZero 服务已验证构造失败回收与完整回归，没有将这三项进程验收泛化为所有服务的信号/在途请求排空验收。验证代码、服务夹具和运行报告均不在本仓库。
