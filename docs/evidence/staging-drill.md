# Staging 备份、恢复与回滚演练

> **历史整栈演练记录，不是当前功能结论。** 当前 v1 的 PostgreSQL + artifact 备份恢复和真实 0.2 → 1.0 升级由普通功能门禁复验；只有项目需要声明 100 Node/24 小时生产规模时，才额外运行同 commit capacity 门禁。边界见[生产就绪证据](../production-readiness.md)。

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

这条记录只证明上述历史提交。本轮代码基线 `2a0d647ad47ccbd1b1834de95b38e55b2ef98229` 尚未重跑
Staging Drill，不能沿用这条成功记录。

脚本以 0 退出时，manifest 会记录 `staging drill = real restore verified`。发布判断还应同时核对对应 run 的
artifact 与 commit，不能只看文档中的状态文字。
