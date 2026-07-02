# Operations Runbook

This runbook is for production candidate / beta operations. Prefer read-only checks first. Use destructive actions only with explicit confirmation.

## Health

Check compose state:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml ps
```

Check service health:

```bash
curl -fsS http://127.0.0.1:8090/health
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8081/health
curl -fsS http://127.0.0.1:8082/health
curl -fsS http://127.0.0.1:8085/health
```

If local health checks return proxy errors, set:

```bash
export NO_PROXY="${NO_PROXY:-localhost,127.0.0.1,::1},localhost,127.0.0.1,::1"
export no_proxy="$NO_PROXY"
```

## Gateway Route Exists

Query orchestrator routes:

```bash
curl -fsS "$ORCHESTRATOR_URL/nodes/child-node/routes?include_upstream=true" | jq .
```

Confirm the expected `api_id`, target service, and required permission are present. If a route is missing, inspect the service `release.yaml`, reinstall the release, and check operation logs.

## Auth Permission Registered

Use the auth database or the auth admin API, depending on production access policy:

```bash
psql "$AUTH_DATABASE_URL" -c "select code from permissions order by code;"
psql "$AUTH_DATABASE_URL" -c "select role_id, permission_code from role_permissions order by permission_code;"
```

If a service permission is missing, verify the release install registered permissions and inspect auth-service migration status.

## Redis Queue Backlog

Use judge-api queue status:

```bash
curl -fsS -H 'X-Auth-Verified: true' -H 'X-Roles: admin' \
  "$JUDGE_API_URL/judge/admin/queue/status" | jq .
```

Direct Redis checks:

```bash
redis-cli -u "$REDIS_URL" XLEN ojos:judge:task
redis-cli -u "$REDIS_URL" XPENDING ojos:judge:task ojos-judge-workers
```

## Worker Consumption

Check worker logs:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml logs --no-color judge-worker
```

Confirm:

- worker registered with judge-api;
- `OJOS_RUNNER_MODE=nsjail`;
- no repeated compile/runtime sandbox failures;
- Redis stream group has active consumers.

## MinIO Object Read/Write

Check bucket and object access:

```bash
mc alias set ojos "$MINIO_ENDPOINT" "$MINIO_ACCESS_KEY" "$MINIO_SECRET_KEY"
mc ls ojos/problems
printf 'probe\n' >/tmp/ojos-minio-probe.txt
mc cp /tmp/ojos-minio-probe.txt ojos/judge-artifacts/probes/ojos-minio-probe.txt
mc cat ojos/judge-artifacts/probes/ojos-minio-probe.txt
```

Also verify storage-service:

```bash
curl -fsS http://127.0.0.1:8085/health | jq .
```

## nsjail Runner

Inside judge-worker image:

```bash
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml exec judge-worker nsjail --help >/tmp/nsjail-help.txt
docker compose --env-file /etc/ojos/production.env -f deploy/compose/docker-compose.yml exec judge-worker cat /opt/ojos/runtime-versions.txt
```

If nsjail is unavailable, do not switch to a fake runner. Rebuild the pinned worker image and keep the service out of production traffic until the nsjail matrix passes.

## Pending Task Recovery

If pending tasks are stuck:

1. Check `judge-api` queue status.
2. Check Redis `XPENDING`.
3. Restart judge-worker once.
4. If a task remains pending beyond lease TTL, claim it using the supported worker recovery path or run the Redis recovery drill in a disposable environment.
5. Do not manually delete stream entries until the submission state and result stream are reconciled.

Reference drill:

```bash
deploy/ops/redis-recovery-drill.sh
```

## Revoke Service Credential

Use the auth service control path if available; otherwise update the auth database through an audited maintenance session:

```sql
update service_credentials
set enabled = false, revoked_at = now(), updated_at = now()
where service_code = '<service-code>' and token_hint = '<token-hint>';
```

Then verify deny behavior with the service credential lifecycle drill in staging:

```bash
deploy/ops/service-credential-drill.sh
```

## Rollback

For operation rollback:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
ORCHESTRATOR_URL="$ORCHESTRATOR_URL" \
OJOS_ROLLBACK_OPERATION_ID="$OPERATION_ID" \
OJOS_CONFIRM_ROLLBACK="rollback-$OPERATION_ID" \
deploy/ops/rollback-drill.sh
```

After rollback, verify host service state, endpoint state, API surface, effective routes, permissions, credentials/grants, and health.

## Backup / Restore

Backup:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/backup.sh
```

Restore:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env \
OJOS_RESTORE_DIR=/var/backups/ojos/<stamp> \
OJOS_CONFIRM_RESTORE=restore-production \
deploy/ops/restore.sh
```

Always run preflight and smoke checks after restore.

## Trace

Run a trace drill:

```bash
deploy/ops/trace-e2e-drill.sh
```

Query Jaeger:

```bash
curl -fsS "$JAEGER_QUERY_URL/api/traces/$TRACE_ID" | jq .
```

Expected services include gateway-service, judge-api-service, storage-service, and judge-worker. Redis Stream propagation is represented by trace metadata and a native judge-worker consumer span.

## Alert Firing

Run:

```bash
deploy/ops/alert-firing-drill.sh
```

Confirm Prometheus rule firing and Alertmanager webhook delivery in the artifact manifest.

## Production Secret Errors

Run:

```bash
OJOS_ENV_FILE=/etc/ojos/production.env deploy/ops/secret-check.sh
```

Common failures:

- missing secret: set the env var or `VAR_FILE`;
- weak placeholder: rotate the value;
- localhost database URL: use production database endpoint;
- Redis URL without password: add password-authenticated URL;
- PostgreSQL superuser: create a least-privilege service user.

After changing secrets, restart affected services and rerun preflight.
