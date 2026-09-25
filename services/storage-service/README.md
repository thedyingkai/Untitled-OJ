# Storage Service

`storage-service` 是 OJOS 的对象存储 provider。Service Contract v3 人工真值由
`ojos.service.yaml`、`api/*.openapi.yaml` 和 `config.schema.json` 组成；`gen/`、
Catalog Release 和客户端均由 `ojos service` 生成。旧 `release.yaml`、根目录
`openapi.yaml`、`service.yaml` 与 `.api` 仅保留一个迁移周期，不得作为 v3 变更入口。

## v3 API 与路由

服务分别提供四个 workload API Binding：

| API ID | 方法 | provider path | 权限 |
| --- | --- | --- | --- |
| `storage.object.put` | `PUT` | `/{bucket}/{key}` | `storage.object.write` |
| `storage.object.get` | `GET` | `/{bucket}/{key}` | `storage.object.read` |
| `storage.object.head` | `HEAD` | `/{bucket}/{key}` | `storage.object.read` |
| `storage.object.delete` | `DELETE` | `/{bucket}/{key}` | `storage.object.delete` |

`/{bucket}/{key}` 是权威 provider path。Gateway 的内部 Binding base 为
`/internal/apis/<api-id>`，调用方只追加上述相对路径。旧
`/api/storage/objects/{bucket}/{key}` 只用于未托管开发环境的迁移兼容。
托管模式下 `storage.object.delete` 必须同时提交预期 SHA-256 与 size，避免 GC
把同名但内容已变化的对象误删；无条件 DELETE 只保留给未托管迁移别名。

`/healthz` 只证明进程存活；`/readyz` 会实际检查 S3 端点及所有配置 bucket。

桶目录使用独立的短时读写锁；健康探针、桶名查询和已发布对象的元数据读取不等待
慢上传读取完请求体。对象变更仍在同一 store 实例内串行执行，本地条件删除在
实际移除前检查请求取消。桶创建成功后才登记到运行时目录，失败不会发布半成品配置。

## 生产配置和身份

生产使用 S3 兼容端点；新的自托管配方使用 SeaweedFS，已有 MinIO 继续兼容。
安装时按 CompositionPlan 提交：

```json
{
  "config": {
    "mode": "production",
    "backend": "s3",
    "buckets": "problems,submissions,judge-artifacts,avatars",
    "s3Endpoint": "s3.internal:443",
    "s3UseSSL": true,
    "s3Region": "us-east-1"
  },
  "secret_refs": {
    "s3AccessKey": "storage-s3-access",
    "s3SecretKey": "storage-s3-secret"
  }
}
```

Agent 将配置展开为 `OJOS_CONFIG_*`，将 secret 展开为 `OJOS_SECRET_*`；secret
明文不进入控制面 Job。`OJOS_MANAGED_WORKLOAD=true` 时服务会先清空镜像内开发配置，
并拒绝 `STORAGE_BACKEND`、`S3_ACCESS_KEY`、`MINIO_ACCESS_KEY` 等未托管变量污染。
`backend: minio` 与 `minioEndpoint/minioUseSSL/minioAccessKey/minioSecretKey` 保留原义；
不能混填两套字段，也不会从 MinIO 配置猜测新 S3 的 endpoint 或凭据。

未托管部署使用 `STORAGE_BACKEND=s3`、`S3_ENDPOINT`、`S3_ACCESS_KEY`、`S3_SECRET_KEY`、
`S3_USE_SSL` 与可选 `S3_REGION`。业务凭据应仅能操作已预建的配置桶。供应端部署、
升级及现有数据切换见[自托管 S3](../../deploy/object-store/README.md)。

对象 API 只接受经 Gateway 转发的短期 Ed25519 workload JWT。服务会再次验证签名，
并要求 JWT 中的 service/node/deployment 与 Gateway 的可信 caller headers、Binding ID
和目标 API ID 完全一致。`/healthz`、`/readyz` 保持匿名。

## 开发与验证

未托管本地开发可使用 `local` backend 和写目录；这条路径不会被 Catalog 的生产
配置模式接受。

验证命令和场景已迁至仓库外的独立验证目录；不在产品工作树内运行。

容器以非 root 用户运行。S3 上传先写入 provider 内部的随机临时 object，完成
SHA-256/size 校验后，再以带条件头的单次流式 PUT 原子发布，并在所有返回路径清理
临时 object；服务不需要本地 spool，因此可直接使用只读根文件系统。
S3 的 `.ojos-upload/` 暂存对象不进入业务列表，也不占用业务分页的条数和游标。
