# Staging 备份/恢复/回滚演练

状态：nightly 可执行演练。

`deploy/ops/staging-drill.sh` 使用一次性 Docker 资源，不触碰生产数据。

它验证：

- PostgreSQL dump、恢复到全新的一次性数据库、行数与行校验和。
- MinIO bucket 创建、样本对象备份、删除、恢复与校验和匹配。
- 针对 `judge-api` 的编排器 release 演练：安装 v1、升级到生成的一次性 v2 manifest、回滚到 v1，并验证
  host service 状态、endpoint 状态、API surface / 有效路由、权限、operation 日志和路由探测。
- 迁移回滚被显式记录为 `schema rollback unsupported; app-level rollback only`（schema 回滚不支持，
  仅应用层回滚）。

证据产物在本地写入 `artifacts/staging-drill/<run-id>/`，并由 `Staging Drill` workflow 上传：

- `manifest.json`
- `logs/staging-drill.log`
- `logs/postgres.log`
- `logs/minio.log`
- `logs/orchestrator-daemon.log`
- `responses/*.json`
- `postgres/staging-drill.dump`
- `minio-backup/sample.txt`
- `minio-restore/sample.txt`

当前远端门禁分类：`pending-first-run`。

当 `deploy/ops/staging-drill.sh` 以 0 退出时，其 manifest 记录 `staging drill = real restore verified`
（staging 演练 = 真实恢复已验证）。
