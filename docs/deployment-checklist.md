# Deployment Checklist

Use this checklist for the first production candidate / beta deployment. Run it in order and stop on any failed P0/P1 item.

## Environment Requirements

- Linux host or WSL2 host with Docker Desktop / Docker Engine.
- Docker Compose v2.
- PostgreSQL 17-compatible server for each production database.
- Redis 8.8-compatible server with password authentication and persistence enabled.
- MinIO `RELEASE.2025-09-07T16-13-09Z` or compatible S3 endpoint.
- `nsjail` available in the judge-worker image; host must support the configured cgroup/seccomp/mount policy.
- Tooling for ops scripts: `bash`, `curl`, `jq`, `docker`, `pg_dump`, `pg_restore`, `redis-cli`, `mc`, `sha256sum`.
- Configure `NO_PROXY=localhost,127.0.0.1,::1` when running local drills or health probes behind a corporate proxy.

## Secret Configuration

Create a production env file from `.env.production.example`, not `.env.example`.

Required production secrets:

- `JWT_SECRET`: at least 32 characters.
- `AUTH_INTERNAL_TOKEN`: at least 32 characters.
- `ORCHESTRATOR_INTERNAL_TOKEN`: at least 32 characters.
- `OJOS_WORKER_TOKEN`: at least 32 characters.
- `AUTH_POSTGRES_PASSWORD`, `PROBLEM_POSTGRES_PASSWORD`, `JUDGE_POSTGRES_PASSWORD`, `USER_POSTGRES_PASSWORD`, `ORCHESTRATOR_POSTGRES_PASSWORD`: at least 20 characters.
- `AUTH_DATABASE_URL`, `PROBLEM_DATABASE_URL`, `JUDGE_DATABASE_URL`, `USER_DATABASE_URL`, `ORCHESTRATOR_DATABASE_URL`: password-authenticated PostgreSQL URLs, not the `postgres` superuser.
- `REDIS_PASSWORD` and `REDIS_URL`: password-authenticated Redis URL.
- `MINIO_ROOT_USER`, `MINIO_ROOT_PASSWORD`, `MINIO_ACCESS_KEY`, `MINIO_SECRET_KEY`.
- Monitoring profile: `OJOS_ALERT_WEBHOOK_URL` and `GRAFANA_ADMIN_PASSWORD` when monitoring is enabled.

Preflight:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh
```

## Startup Steps

1. Install Docker / Docker Compose and confirm the daemon is running.
2. Put the production env file at `/etc/ojos/production.env` with mode `0600`.
3. Run preflight:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/preflight.sh
```

4. Build or pull the pinned images:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml build \
  orchestrator auth-service storage-service gateway problem-service judge-api judge-worker user-service
```

5. Start databases and infrastructure.
6. Run migrations before opening traffic.
7. Start services:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml up -d
```

## Migration Steps

Run migration services explicitly:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm orchestrator-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm auth-service-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm problem-service-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm judge-api-migrations
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml run --rm user-service-migrations
```

Do not run destructive migrations without a fresh backup and an explicit rollback plan.

## Smoke Verification

Run:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml ps
curl -fsS http://127.0.0.1:8090/health
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8081/health
curl -fsS http://127.0.0.1:8082/health
curl -fsS http://127.0.0.1:8085/health
```

Then run a judge smoke through the deployed gateway or the existing compose smoke command for the environment.

## Rollback Steps

For an operation rollback:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL=https://orchestrator.example.com \
OJOS_ROLLBACK_OPERATION_ID=op-release-install-YYYYMMDD \
OJOS_CONFIRM_ROLLBACK=rollback-op-release-install-YYYYMMDD \
deploy/ops/rollback-drill.sh
```

If schema rollback is needed, stop and use backup/restore. Current release rollback is app-level; schema rollback is unsupported.

## Backup / Restore

Backup:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/backup.sh
```

Restore requires explicit confirmation:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/20260702T120000Z \
OJOS_CONFIRM_RESTORE=restore-production \
deploy/ops/restore.sh
```

After restore, run preflight and smoke checks before reopening traffic.

## Logs

- Compose service logs: `docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml logs --no-color <service>`.
- Drill artifacts: `artifacts/<drill-name>/`.
- Orchestrator operation logs: query the orchestrator operation detail endpoint or manager smoke responses.
- Prometheus / Alertmanager / Jaeger logs: monitoring compose logs.

## Monitoring And Alerts

Start monitoring:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
docker compose --env-file /etc/ojos/production.env -f deploy/ops/monitoring/docker-compose.yml up -d
```

Verify:

```bash
deploy/ops/alert-firing-drill.sh
deploy/ops/trace-e2e-drill.sh
```

## Common Troubleshooting

| Symptom | Check | Action |
| --- | --- | --- |
| Docker daemon unavailable | `docker ps` | Start Docker / service manager and rerun preflight |
| proxy intercepts local curl | `env | grep -i proxy` | Set `NO_PROXY=localhost,127.0.0.1,::1` |
| nsjail unavailable | `docker compose logs judge-worker` | Rebuild judge-worker image and confirm runtime lock |
| Redis unavailable | `redis-cli -u "$REDIS_URL" ping` | Check password, network, persistence, and stream group |
| MinIO unavailable | `curl /minio/health/live` or `mc ls` | Check credentials, endpoint, bucket, and policy |
| gateway route missing | Orchestrator route table | Reinstall release or reload gateway routes |
| permission denied | auth permissions / service grants | Verify release permissions and service credential grants |
| worker pending not consumed | judge queue status API | Check worker registration, Redis stream group, nsjail failures, and worker token |
