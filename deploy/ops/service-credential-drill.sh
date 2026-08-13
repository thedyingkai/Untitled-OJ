#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_CREDENTIAL_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/service-credential-drill/$run_id}"
mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd)"
mkdir -p "$evidence_dir/logs"
log_file="$evidence_dir/logs/service-credential-drill.log"
exec > >(tee -a "$log_file") 2>&1

container="ojos-credential-drill-postgres-$run_id"
pg_user="ojos_credential_drill"
pg_password="OjosCredentialPg_0123456789abcdef"
pg_db="ojos_credential_drill"
postgres_image="${OJOS_DRILL_POSTGRES_IMAGE:-postgres:17}"
postgres_ready_timeout="${OJOS_CREDENTIAL_DRILL_READY_TIMEOUT_SECONDS:-120}"
postgres_init_complete="PostgreSQL init process complete; ready for start up."
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    cat >&2 <<EOF
[ENV-BLOCKED] service-credential-drill
命令：command -v $1
错误摘要：$1 not found
判断：环境问题
是否阻塞当前任务：是
最小修复建议：安装或启动缺失依赖后重跑 service credential lifecycle drill
后续处理：需要用户介入
EOF
    exit 127
  }
}

sha256_text() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | awk '{print $1}'
  else
    printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
  fi
}

docker_exec() {
  MSYS2_ARG_CONV_EXCL='*' docker exec "$@"
}

container_is_running() {
  [[ "$(docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" == "true" ]]
}

final_postgres_is_pid_one() {
  docker_exec "$container" sh -ec 'test "$(cat /proc/1/comm)" = postgres' >/dev/null 2>&1
}

wait_for_final_postgres() {
  local deadline=$((SECONDS + postgres_ready_timeout))

  # postgres' official entrypoint starts a temporary server during initdb and
  # stops it before exec-ing the final server. pg_isready alone can therefore
  # succeed inside a short-lived window. This marker is emitted only after the
  # temporary server has completed its shutdown.
  while ((SECONDS < deadline)); do
    if docker logs "$container" 2>&1 | grep -F "$postgres_init_complete" >/dev/null; then
      break
    fi
    if ! container_is_running; then
      echo "PostgreSQL container exited before initdb completed" >&2
      return 1
    fi
    sleep 1
  done
  if ! docker logs "$container" 2>&1 | grep -F "$postgres_init_complete" >/dev/null; then
    echo "Timed out waiting for PostgreSQL initdb temporary server shutdown" >&2
    return 1
  fi

  # The marker precedes the entrypoint's final exec by a few instructions.
  # Require PID 1 to have become postgres and then take a fresh readiness
  # observation. The temporary initdb server is a child of the entrypoint and
  # therefore cannot satisfy this identity gate.
  while ((SECONDS < deadline)); do
    if final_postgres_is_pid_one && \
      docker_exec "$container" pg_isready -U "$pg_user" -d "$pg_db" >/dev/null 2>&1; then
      return 0
    fi
    if ! container_is_running; then
      echo "PostgreSQL container exited before final server readiness" >&2
      return 1
    fi
    sleep 1
  done
  echo "Timed out waiting for final PostgreSQL server readiness" >&2
  return 1
}

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker logs "$container" >"$evidence_dir/logs/postgres.log" 2>&1 || true
  if [[ "${OJOS_DRILL_KEEP_CONTAINERS:-0}" != "1" ]]; then
    docker rm -f "$container" >/dev/null 2>&1 || true
  fi
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg matrix "allow-deny-revoke-expire" \
    '{
      drill: "service-credential-lifecycle",
      status: $status,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      matrix: $matrix,
      evidence: {
        log: "logs/service-credential-drill.log",
        results: "matrix.json"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] service credential lifecycle allow/deny/revoke/expire matrix passed"
  else
    echo "[FAILED] service credential lifecycle drill failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

need_cmd docker
need_cmd jq

case "$postgres_ready_timeout" in
  '' | *[!0-9]*)
    echo "OJOS_CREDENTIAL_DRILL_READY_TIMEOUT_SECONDS must be a positive integer" >&2
    exit 64
    ;;
esac
if ((postgres_ready_timeout < 1)); then
  echo "OJOS_CREDENTIAL_DRILL_READY_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 64
fi

docker run -d \
  --name "$container" \
  -e "POSTGRES_USER=$pg_user" \
  -e "POSTGRES_PASSWORD=$pg_password" \
  -e "POSTGRES_DB=$pg_db" \
  "$postgres_image" >/dev/null

wait_for_final_postgres

psql() {
  docker_exec -i -e "PGPASSWORD=$pg_password" "$container" \
    psql -v ON_ERROR_STOP=1 -U "$pg_user" -d "$pg_db" "$@"
}

for migration in "$repo_root"/services/auth-service/migrations/*.up.sql; do
  name="$(basename "$migration")"
  docker cp "$migration" "$container:/tmp/$name"
  psql --single-transaction -f "/tmp/$name"
done

valid_token="service-valid-token-$run_id"
expired_token="service-expired-token-$run_id"
revoked_token="service-revoked-token-$run_id"
wrong_token="service-wrong-token-$run_id"
no_grant_token="service-no-grant-token-$run_id"
rollback_token="service-rollback-token-$run_id"
valid_hash="$(sha256_text "$valid_token")"
expired_hash="$(sha256_text "$expired_token")"
revoked_hash="$(sha256_text "$revoked_token")"
wrong_hash="$(sha256_text "$wrong_token")"
no_grant_hash="$(sha256_text "$no_grant_token")"
rollback_hash="$(sha256_text "$rollback_token")"

psql <<SQL
INSERT INTO permissions(code, service_code, name, description)
VALUES
  ('storage.object.read', 'storage-service', 'Storage Read', 'drill read'),
  ('storage.object.write', 'storage-service', 'Storage Write', 'drill write'),
  ('storage.object.delete', 'storage-service', 'Storage Delete', 'drill delete')
ON CONFLICT(code) DO UPDATE SET service_code = EXCLUDED.service_code;

INSERT INTO service_identities(service_code, enabled)
VALUES ('judge-api', TRUE), ('no-grant-service', TRUE), ('revoked-by-rollback', TRUE)
ON CONFLICT(service_code) DO UPDATE SET enabled = TRUE, updated_at = NOW();

INSERT INTO service_credentials(service_code, token_hash, token_hint, enabled, expires_at, revoked_at)
VALUES
  ('judge-api', '$valid_hash', 'valid', TRUE, NOW() + INTERVAL '1 hour', NULL),
  ('judge-api', '$expired_hash', 'expired', TRUE, NOW() - INTERVAL '1 hour', NULL),
  ('judge-api', '$revoked_hash', 'revoked', FALSE, NOW() + INTERVAL '1 hour', NOW()),
  ('no-grant-service', '$no_grant_hash', 'valid', TRUE, NOW() + INTERVAL '1 hour', NULL),
  ('revoked-by-rollback', '$rollback_hash', 'valid', TRUE, NOW() + INTERVAL '1 hour', NULL)
ON CONFLICT(service_code, token_hash) DO UPDATE
SET enabled = EXCLUDED.enabled,
    expires_at = EXCLUDED.expires_at,
    revoked_at = EXCLUDED.revoked_at,
    updated_at = NOW();

INSERT INTO service_permission_grants(caller_service_code, api_id, permission_code, provider_service_code, enabled)
VALUES
  ('judge-api', 'storage.object.get', 'storage.object.read', 'storage-service', TRUE),
  ('revoked-by-rollback', 'storage.object.get', 'storage.object.read', 'storage-service', TRUE)
ON CONFLICT(caller_service_code, api_id, permission_code) DO UPDATE
SET enabled = TRUE, updated_at = NOW();
SQL

can_use() {
  local service="$1"
  local permission="$2"
  local api_id="$3"
  local token_hash="$4"
  psql -tAc "SELECT EXISTS (
    SELECT 1
    FROM service_identities si
    JOIN service_credentials sc ON sc.service_code = si.service_code
    JOIN service_permission_grants spg ON spg.caller_service_code = si.service_code
    JOIN permissions p ON p.code = spg.permission_code
    WHERE si.service_code = '$service'
      AND si.enabled
      AND sc.enabled
      AND sc.token_hash = '$token_hash'
      AND sc.revoked_at IS NULL
      AND (sc.expires_at IS NULL OR sc.expires_at > NOW())
      AND spg.enabled
      AND spg.permission_code = '$permission'
      AND ('$api_id' = '' OR spg.api_id = '$api_id')
  );" | tr -d '[:space:]'
}

record_result() {
  local name="$1"
  local expected="$2"
  local actual="$3"
  jq -n --arg name "$name" --arg expected "$expected" --arg actual "$actual" \
    '{case: $name, expected: $expected, actual: $actual, passed: ($expected == $actual)}'
}

valid_allow="$(can_use judge-api storage.object.read storage.object.get "$valid_hash")"
if [[ "$valid_allow" == "t" ]]; then
  psql -c "UPDATE service_credentials SET last_used_at = NOW(), updated_at = NOW() WHERE service_code = 'judge-api' AND token_hash = '$valid_hash';"
fi
expired_deny="$(can_use judge-api storage.object.read storage.object.get "$expired_hash")"
revoked_deny="$(can_use judge-api storage.object.read storage.object.get "$revoked_hash")"
unknown_deny="$(can_use unknown-service storage.object.read storage.object.get "$valid_hash")"
wrong_deny="$(can_use judge-api storage.object.read storage.object.get "$wrong_hash")"
no_grant_deny="$(can_use no-grant-service storage.object.read storage.object.get "$no_grant_hash")"
last_used="$(psql -tAc "SELECT COALESCE(to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), '') FROM service_credentials WHERE service_code = 'judge-api' AND token_hash = '$valid_hash';")"

psql -c "UPDATE service_credentials SET enabled = FALSE, revoked_at = NOW(), updated_at = NOW() WHERE service_code = 'revoked-by-rollback' AND token_hash = '$rollback_hash';"
rollback_revoke_deny="$(can_use revoked-by-rollback storage.object.read storage.object.get "$rollback_hash")"

{
  echo '['
  record_result "valid token allow" "t" "$valid_allow"
  echo ','
  record_result "expired token deny" "f" "$expired_deny"
  echo ','
  record_result "revoked token deny" "f" "$revoked_deny"
  echo ','
  record_result "unknown service deny" "f" "$unknown_deny"
  echo ','
  record_result "wrong token deny" "f" "$wrong_deny"
  echo ','
  record_result "no grant deny" "f" "$no_grant_deny"
  echo ','
  record_result "release.rollback revoke credential" "f" "$rollback_revoke_deny"
  echo ','
  jq -n --arg name "last_used_at updated" --arg value "$last_used" \
    '{case: $name, expected: "non-empty", actual: $value, passed: ($value != "")}'
  echo ']'
} >"$evidence_dir/matrix.json"

jq -e 'all(.[]; .passed == true)' "$evidence_dir/matrix.json" >/dev/null
