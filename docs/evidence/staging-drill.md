# Staging 备份、恢复与回滚演练

`deploy/ops/staging-drill.sh` 是 nightly 的一次性演练，不连接生产资源。

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

最近一次已核对的远端成功运行：

- workflow：`Staging Drill`
- run：[`30717233049`](https://github.com/thedyingkai/Untitled-OJ/actions/runs/30717233049)
- commit：`875586ff92324d8d936d71f35c24cb0f1ad494f5`
- 时间：2026-08-01 20:33 UTC

这条记录只证明上述 `main` 提交。当前 Web、生命周期和鉴权重构必须在推送后重新运行，不能沿用这条成功记录。

脚本以 0 退出时，manifest 会记录 `staging drill = real restore verified`。发布判断还应同时核对对应 run 的
artifact 与 commit，不能只看文档中的状态文字。
