# 自托管 S3 对象存储

OJOS 的新自托管部署使用 **SeaweedFS 4.47 + 标准 S3 接口**。对象 API、题包摘要、
workload 身份和业务数据格式不随供应端改变。Storage 的 `minio` 配置入口保留兼容；
已有 MinIO 不会自动迁移、删除或挂载到 SeaweedFS。

## 选择与边界

截至 2026-09-24，原 MinIO Docker Hub 镜像已在本机实际拉取失败，
[上游仓库](https://github.com/minio/minio)也已归档并声明不再维护。自构建能解决镜像
分发，却不能替代上游安全维护，因此不作为新部署的长期方案。

[SeaweedFS 4.47](https://github.com/seaweedfs/seaweedfs/releases/tag/4.47)仍在维护，
可以提供当前 Storage 所需的 S3 条件写入、多段上传和对象元数据。镜像固定到
`sha256:ce9e796f1fe6f06968f4c04bdaf8f678dad9c8acdfef3d244133d71bfa6bf882`，不使用
`latest`。现有 `minio-go` 包只是 S3 客户端，保留在适配器内，不意味着仍要求 MinIO 服务端。

这里提供的是单节点持久部署，不是多节点高可用。生产跨主机访问需要已有的 HTTPS
入口、可信 CA、备份与容量规划；不能把内部 HTTP 端口公开到互联网。需要托管 S3
或独立 HA 集群时，直接给 Storage 配置该端点，不必安装本配方。

## 安全与职责

- `internal/store/s3_store.go` 负责 S3 对象操作；不创建管理账号或修改桶策略。
- `cmd/ojos-object-store` 是部署工具。`render` 原子生成私有权限文件；`ready` 使用
  已签名请求确认服务可用；`provision` 创建指定桶及内部临时上传清理规则。
- 缺失、过短、占位或复用的管理/业务凭据会使入口在开放端口前失败。支持同名
  `_FILE` 输入，禁止同时设置值与文件。凭据不会打印到生成命令输出。
- 业务身份没有原生 `Read/Write` 通配权限，只通过显式 S3 action 白名单访问指定
  桶。不能创建/删除桶、修改桶 policy 或 lifecycle。管理凭据不下发给 Storage。
- SeaweedFS 的所有 HTTP/gRPC 仅监听容器内 `127.0.0.1`。固定入口只转发 S3 HTTP，
  由供应端验证签名；保留 Host，不添加管理凭据；
  S3 的内部 IAM cache gRPC 也不会暴露到业务网络。IAM 写接口、Iceberg、Lance 与目录浏览关闭，容器使用非 root、只读根文件
  系统和持久 `/data`。
- 部署入口统一持有 SeaweedFS 子进程与 S3 HTTP 入口。任何一方退出都清理另一方；
  正常关闭先排空 HTTP 请求，再停止并回收子进程。避免共享命名空间的独立 sidecar
  在后端重启后继续指向失效网络。
- 初始化只追加名为 `ojos-incomplete-upload` 的规则，清理 `.ojos-upload/` 下的一日
  临时对象及未完成上传；保留已有运维规则，不对 submissions/problem 等业务对象
  自动设置 30 日过期。业务对象删除仍由业务保留/GC 规则决定。

## 启用新实例

在仓库外创建权限受限的环境文件，例如 `/etc/ojos/object-store.env`，填写四个独立
随机值：`S3_ADMIN_ACCESS_KEY`、`S3_ADMIN_SECRET_KEY`、`S3_ACCESS_KEY`、`S3_SECRET_KEY`。
Access key 至少 16 字符，secret 至少 32 字符。生产优先使用只读 secret 文件挂载。
这不是 `.env.production.example` 的替代品，不要将整套生产密钥注入对象存储容器。

设置 `OJOS_OBJECT_STORE_ENV_FILE` 为该文件的绝对路径。另把同一业务账号的
`S3_ACCESS_KEY` / `S3_SECRET_KEY` 提供给 Compose 插值环境，供显式开发 Storage 覆盖使用。
不要给这两个变量填管理凭据。

从仓库根目录，显式叠加对象存储配置：

```bash
docker compose --env-file /etc/ojos/production.env \
  --env-file /etc/ojos/object-store.env \
  -f deploy/compose/docker-compose.yml \
  -f deploy/compose/docker-compose.s3.yml \
  up -d --build object-store object-store-init
```

新卷名为 `s3-data`。基础 Compose 不再自动启动旧 MinIO，也不改变原 Storage 的
`local` 默认值。只有显式叠加此文件才选择新 S3；仅修改源码或更新基础 Compose
不会把旧 Storage 自动指向空的新卷。基础文件保留 `minio-data` 声明以方便识别历史
数据，升级时不要运行 `down -v` 或 `--remove-orphans`。

Store/Agent 正式部署使用 [Storage 的 v3 配置](../../services/storage-service/README.md)，
引用可达的 HTTPS S3 地址及业务 secret refs；无需启用 `legacy-development`。该
Compose 覆盖里的静态 `storage-service` 仍只属于显式开发兼容 profile。

## 从现有 MinIO 切换

1. 记录源实例、桶、对象数/总字节、版本化、对象锁、过期策略、加密和权限设置。
   若使用对象版本、锁或 provider KMS，先设计相应迁移，不能按普通对象复制处理。
2. 建立独立目标实例和新卷，不复用 MinIO 的内部磁盘目录。关闭目标的业务写入。
3. 用 S3→S3 的复制工具保留对象 key、内容、Content-Type 和全部用户元数据，尤其
   是 `ojos-sha256` 与 `ojos-updated-at`；不要通过普通本地文件目录中转而丢失元数据。
   [rclone 的 S3 后端](https://rclone.org/s3/)支持元数据复制，可使用 `copy --metadata`
   处理明确选择的桶；配置与凭据放在仓库外。不要使用带删除语义的 `sync`。
4. 暂停 Storage 写入及相关 GC/业务写入者，再做增量复制。使用
   [内容下载校验](https://rclone.org/commands/rclone_check/)而非只比 ETag：多段对象的
   ETag 不是普通内容摘要。另逐项核对自定义元数据和内容类型。
5. 单独重建并检查权限、生命周期、加密等控制配置；对象复制不会自动迁移这些配置。
6. 更新 Storage 的 endpoint 与 secret refs，确认题包/源码/头像/判题产物可读及新写入
   正常后再解除写入暂停。保留源存储只读和迁移清单。
7. 切换前可以直接回到源；目标开始接受新写入后，不能仅改回旧 endpoint，需要先
   暂停并核对反向增量。不要在本批代码部署中自动执行数据清理。

此处是运维切换步骤，不表示已迁移任何现有用户数据。本机兼容验收只操作隔离实例。

## 备份兼容

`backup.sh` / `restore.sh` 接受 `S3_ENDPOINT`、`S3_ACCESS_KEY`、`S3_SECRET_KEY`、
`S3_USE_SSL`。备份 v1 的 `storage.minio` 字段和目录名为历史格式，不改写旧档案。
恢复到空实例时需使用独立、受控的恢复管理员，而非运行时业务账号；v1 运维工具链
仍使用既有 `mc`，应由运维环境提供受控版本。上述跨 provider 迁移必须另保留元数据，
不能把旧文件镜像备份当作完整 S3 迁移工具。

软件验证、夹具和报告全部在仓库外；workflow 只编译和打包本配方，不运行验收场景。
