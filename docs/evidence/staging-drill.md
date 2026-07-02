# Staging Backup/Restore/Rollback Drill

Status: nightly executable drill.

`deploy/ops/staging-drill.sh` uses disposable Docker resources and does not touch production data.

It verifies:

- PostgreSQL dump, restore into a fresh disposable database, row count, and row checksum.
- MinIO bucket creation, sample object backup, delete, restore, and checksum match.
- Orchestrator release drill for `judge-api`: install v1, upgrade to a generated disposable v2 manifest, rollback to v1, and verify host service state, endpoint state, API surface/effective route, permissions, operation logs, and route probe.
- Migration rollback is explicitly recorded as `schema rollback unsupported; app-level rollback only`.

Evidence artifacts are written under `artifacts/staging-drill/<run-id>/` locally and uploaded by the `Staging Drill` workflow:

- `manifest.json`
- `logs/staging-drill.log`
- `logs/postgres.log`
- `logs/minio.log`
- `logs/orchestrator-daemon.log`
- `responses/*.json`
- `postgres/staging-drill.dump`
- `minio-backup/sample.txt`
- `minio-restore/sample.txt`

Current remote gate classification: `pending-first-run`.

When `deploy/ops/staging-drill.sh` exits 0, its manifest records `staging drill = real restore verified`.
