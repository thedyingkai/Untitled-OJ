# OJOS Production Ops

This directory contains the production gates for deployment operations.

Required checks before opening traffic:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh
```

Back up all state:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/backup.sh
```

Restore is intentionally guarded:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/20260702T120000Z \
OJOS_CONFIRM_RESTORE=restore-production \
deploy/ops/restore.sh
```

Run a real rollback drill against Orchestrator:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL=https://orchestrator.example.com \
OJOS_ROLLBACK_OPERATION_ID=op-release-install-20260702 \
OJOS_CONFIRM_ROLLBACK=rollback-op-release-install-20260702 \
deploy/ops/rollback-drill.sh
```

Monitoring:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
docker compose --env-file /etc/ojos/production.env -f deploy/ops/monitoring/docker-compose.yml up -d
```

`secret-check.sh` accepts either direct env values or `VAR_FILE` paths for values supplied by a secret manager. It rejects empty values, committed local defaults, placeholder values, localhost database URLs, and the PostgreSQL superuser in production.
