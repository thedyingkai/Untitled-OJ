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

`/healthz` 只证明进程存活；`/readyz` 会实际检查 MinIO 及所有配置 bucket。

## 生产配置和身份

生产只能使用 MinIO。安装时按 CompositionPlan 提交：

```json
{
  "config": {
    "mode": "production",
    "backend": "minio",
    "buckets": "problems,submissions,judge-artifacts,avatars",
    "minioEndpoint": "minio.internal:9000",
    "minioUseSSL": true
  },
  "secret_refs": {
    "minioAccessKey": "storage-minio-access",
    "minioSecretKey": "storage-minio-secret"
  }
}
```

Agent 将配置展开为 `OJOS_CONFIG_*`，将 secret 展开为 `OJOS_SECRET_*`；secret
明文不进入控制面 Job。`OJOS_MANAGED_WORKLOAD=true` 时服务会先清空镜像内开发配置，
并拒绝 `STORAGE_BACKEND`、`MINIO_ACCESS_KEY`、`MINIO_SECRET_KEY` 等旧变量污染。

对象 API 只接受经 Gateway 转发的短期 Ed25519 workload JWT。服务会再次验证签名，
并要求 JWT 中的 service/node/deployment 与 Gateway 的可信 caller headers、Binding ID
和目标 API ID 完全一致。`/healthz`、`/readyz` 保持匿名。

## 开发与验证

未托管本地开发可使用 `local` backend 和写目录；这条路径不会被 Catalog 的生产
配置模式接受。

```powershell
Workload verifier trust is a platform projection, not an install input. Store
loads the public Ed25519 key and trust tuple, the Agent atomically writes
`/run/ojos/service/workload-public-key.pem`, and Runtime force-injects the fixed
file path, key ID, issuer, and audience. A service author cannot replace this
material through image configuration or submitted secrets.

cargo run -p ojos-service -- service build services/storage-service/ojos.service.yaml
cargo run -p ojos-service -- service check services/storage-service/ojos.service.yaml --generated
go test -race ./...
go vet ./...
pwsh services/storage-service/scripts/resolved-artifacts-fixture.test.ps1
pwsh services/storage-service/scripts/publish-fixture.test.ps1
```

容器以非 root 用户运行。MinIO 上传先写入 provider 内部的随机临时 object，完成
SHA-256/size 校验后，再以带条件头的单次流式 PUT 原子发布，并在所有返回路径清理
临时 object；服务不需要本地 spool，因此可直接使用只读根文件系统。
