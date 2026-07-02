#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_CREDENTIAL_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/service-credential-drill/$run_id}"
mkdir -p "$evidence_dir/logs"
log_file="$evidence_dir/logs/service-credential-drill.log"
exec > >(tee -a "$log_file") 2>&1

container="ojos-credential-drill-postgres-$run_id"
pg_user="ojos_credential_drill"
pg_password="OjosCredentialPg_0123456789abcdef"
pg_db="ojos_credential_drill"
postgres_image="${OJOS_DRILL_POSTGRES_IMAGE:-postgres:17}"
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "[ENV-BLOCKED] $1" >&2
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

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker logs "$container" >"$evidence_dir/logs/postgres.log" 2>&1 || true
  docker rm -f "$container" >/dev/null 2>&1 || true
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

docker run -d \
  --name "$container" \
  -e "POSTGRES_USER=$pg_user" \
  -e "POSTGRES_PASSWORD=$pg_password" \
  -e "POSTGRES_DB=$pg_db" \
  "$postgres_image" >/dev/null

for _ in $(seq 1 60); do
  if docker exec "$container" pg_isready -U "$pg_user" -d "$pg_db" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec "$container" pg_isready -U "$pg_user" -d "$pg_db" >/dev/null

psql() {
  docker exec -e "PGPASSWORD=$pg_password" "$container" \
    psql -v ON_ERROR_STOP=1 -U "$pg_user" -d "$pg_db" "$@"
}

for migration in "$repo_root"/services/auth-service/migrations/*.up.sql; do
  name="$(basename "$migration")"
  docker cp "$migration" "$container:/tmp/$name"
  psql -f "/tmp/$name"
done

valid_token="service-valid-token-$run_id"
expired_token="service-expired-token-$run_id"
revoked_token="service-revoked-token-$run_id"
wrong_token="service-wrong-token-$run_id"
valid_hash="$(sha256_text "$valid_token")"
expired_hash="$(sha256_text "$expired_token")"
revoked_hash="$(sha256_text "$revoked_token")"
wrong_hash="$(sha256_text "$wrong_token")"

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
  ('no-grant-service', '$valid_hash', 'valid', TRUE, NOW() + INTERVAL '1 hour', NULL),
  ('revoked-by-rollback', '$valid_hash', 'valid', TRUE, NOW() + INTERVAL '1 hour', NULL)
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
no_grant_deny="$(can_use no-grant-service storage.object.read storage.object.get "$valid_hash")"
last_used="$(psql -tAc "SELECT COALESCE(to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), '') FROM service_credentials WHERE service_code = 'judge-api' AND token_hash = '$valid_hash';")"

psql -c "UPDATE service_credentials SET enabled = FALSE, revoked_at = NOW(), updated_at = NOW() WHERE service_code = 'revoked-by-rollback' AND token_hash = '$valid_hash';"
rollback_revoke_deny="$(can_use revoked-by-rollback storage.object.read storage.object.get "$valid_hash")"

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
