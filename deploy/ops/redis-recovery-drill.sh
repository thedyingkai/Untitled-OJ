#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_REDIS_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/redis-recovery-drill/$run_id}"
mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd)"
mkdir -p "$evidence_dir/logs"
log_file="$evidence_dir/logs/redis-recovery-drill.log"
exec > >(tee -a "$log_file") 2>&1

container="ojos-redis-recovery-$run_id"
pg_container="ojos-redis-recovery-postgres-$run_id"
redis_image="${OJOS_DRILL_REDIS_IMAGE:-redis:8.8.0}"
postgres_image="${OJOS_DRILL_POSTGRES_IMAGE:-postgres:17}"
stream="ojos:judge:task:drill:$run_id"
result_stream="ojos:judge:result:drill:$run_id"
group="ojos-judge-workers"
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
pg_user="ojos_drill"
pg_password="OjosDrillPg_0123456789abcdef"
pg_db="ojos_judge_drill"
judge_api_port="${OJOS_REDIS_DRILL_JUDGE_API_PORT:-18082}"
judge_api_pid=""
judge_api_config="$evidence_dir/judge-api.yaml"
queue_status_api="not-run"

docker_exec() {
  MSYS2_ARG_CONV_EXCL='*' docker exec "$@"
}

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker logs "$container" >"$evidence_dir/logs/redis.log" 2>&1 || true
  docker logs "$pg_container" >"$evidence_dir/logs/postgres.log" 2>&1 || true
  if [[ -n "$judge_api_pid" ]]; then
    kill "$judge_api_pid" >/dev/null 2>&1 || true
    wait "$judge_api_pid" >/dev/null 2>&1 || true
  fi
  docker rm -f "$container" "$pg_container" >/dev/null 2>&1 || true
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg stream "$stream" \
    --arg result_stream "$result_stream" \
    --arg queue_status_api "$queue_status_api" \
    '{
      drill: "redis-pending-recovery",
      status: $status,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      task_stream: $stream,
      result_stream: $result_stream,
      persistence: "appendonly yes verified across docker restart",
      queue_status_api: $queue_status_api,
      evidence: {
        log: "logs/redis-recovery-drill.log",
        result: "result.json",
        queue_status: "queue-status.json"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] Redis pending/recovery drill passed"
  else
    echo "[FAILED] Redis pending/recovery drill failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

command -v docker >/dev/null 2>&1 || { echo "[ENV-BLOCKED] docker" >&2; exit 127; }
command -v jq >/dev/null 2>&1 || { echo "[ENV-BLOCKED] jq" >&2; exit 127; }
command -v go >/dev/null 2>&1 || { echo "[ENV-BLOCKED] go" >&2; exit 127; }
command -v curl >/dev/null 2>&1 || { echo "[ENV-BLOCKED] curl" >&2; exit 127; }

docker run -d \
  --name "$container" \
  -p 127.0.0.1::6379 \
  "$redis_image" redis-server --appendonly yes >/dev/null
for _ in $(seq 1 60); do
  if docker_exec "$container" redis-cli ping >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker_exec "$container" redis-cli ping >/dev/null
redis_port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "6379/tcp") 0).HostPort}}' "$container")"
redis_url="redis://127.0.0.1:$redis_port/0"

docker run -d \
  --name "$pg_container" \
  -p 127.0.0.1::5432 \
  -e "POSTGRES_USER=$pg_user" \
  -e "POSTGRES_PASSWORD=$pg_password" \
  -e "POSTGRES_DB=$pg_db" \
  "$postgres_image" >/dev/null
for _ in $(seq 1 60); do
  if docker_exec "$pg_container" pg_isready -U "$pg_user" -d "$pg_db" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker_exec "$pg_container" pg_isready -U "$pg_user" -d "$pg_db" >/dev/null
pg_port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$pg_container")"
database_url="postgres://$pg_user:$pg_password@127.0.0.1:$pg_port/$pg_db?sslmode=disable"

psql() {
  docker_exec -e "PGPASSWORD=$pg_password" "$pg_container" \
    psql -v ON_ERROR_STOP=1 -U "$pg_user" -d "$pg_db" "$@"
}

for migration in "$repo_root"/services/judge-api/migrations/*.up.sql; do
  docker cp "$migration" "$pg_container:/tmp/$(basename "$migration")"
  psql -f "/tmp/$(basename "$migration")"
done

redis() {
  docker_exec "$container" redis-cli "$@"
}

redis XGROUP CREATE "$stream" "$group" 0 MKSTREAM >/dev/null
entry_id="$(redis XADD "$stream" '*' type submission.created task_id sub-1 submission_id 1)"
redis XREADGROUP GROUP "$group" worker-a COUNT 1 STREAMS "$stream" '>' >"$evidence_dir/worker-a-read.txt"
pending_before="$(redis XPENDING "$stream" "$group" | awk 'NR==1 {print $1}')"
[[ "$pending_before" == "1" ]]

redis XAUTOCLAIM "$stream" "$group" worker-b 0 0-0 COUNT 1 >"$evidence_dir/worker-b-autoclaim.txt"
redis XACK "$stream" "$group" "$entry_id" >/dev/null
redis XADD "$result_stream" '*' task_id sub-1 worker_id worker-b status ACCEPTED >/dev/null
pending_after="$(redis XPENDING "$stream" "$group" | awk 'NR==1 {print $1}')"
[[ "$pending_after" == "0" ]]

docker restart "$container" >/dev/null
for _ in $(seq 1 60); do
  if docker_exec "$container" redis-cli ping >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker_exec "$container" redis-cli ping >/dev/null
result_len="$(redis XLEN "$result_stream" | tr -d '[:space:]')"
[[ "$result_len" == "1" ]]
redis_port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "6379/tcp") 0).HostPort}}' "$container")"
redis_url="redis://127.0.0.1:$redis_port/0"

jq -n \
  --arg entry_id "$entry_id" \
  --arg pending_before "$pending_before" \
  --arg pending_after "$pending_after" \
  --arg result_len "$result_len" \
  '{
    task_entry_id: $entry_id,
    worker_stopped_before_ack: true,
    pending_before_recovery: ($pending_before | tonumber),
    recovered_by: "XAUTOCLAIM worker-b",
    pending_after_ack: ($pending_after | tonumber),
    result_stream_length_after_restart: ($result_len | tonumber)
  }' >"$evidence_dir/result.json"

cat >"$judge_api_config" <<YAML
Name: judge-api-redis-recovery-drill
Host: 127.0.0.1
Port: $judge_api_port

Database:
  Url: "$database_url"

Redis:
  Url: "$redis_url"

Jaeger:
  Endpoint: ""

AuthService:
  Endpoint: ""
  AdminToken: ""

Storage:
  SubmissionsRoot: "$evidence_dir/submissions"
  InternalGatewayEndpoint: ""
  GetApiID: storage.object.get
  PutApiID: storage.object.put
  HeadApiID: storage.object.head
  Bucket: submissions
  CallerService: judge-api
  CallerNodeID: ""
  ServiceToken: ""

Submission:
  MaxCodeBytes: 262144

Languages:
  Items: []

WorkerAuth:
  Token: "$run_id-worker-token"
  LeaseTTLSeconds: 60

InternalAuth:
  Enabled: false
  TimestampSkewSeconds: 60
  NonceTTLSeconds: 120
YAML

mkdir -p "$evidence_dir/submissions"
(
  cd "$repo_root/services/judge-api"
  JUDGE_DATABASE_URL="$database_url" \
  REDIS_URL="$redis_url" \
  JAEGER_ENDPOINT="" \
  OJOS_JUDGE_TASK_STREAM="$stream" \
  OJOS_JUDGE_RESULT_STREAM="$result_stream" \
  OJOS_JUDGE_CONSUMER_GROUP="$group" \
  go run . -f "$judge_api_config"
) >"$evidence_dir/logs/judge-api.log" 2>&1 &
judge_api_pid="$!"

for _ in $(seq 1 120); do
  if curl -fsS "http://127.0.0.1:$judge_api_port/health" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$judge_api_pid" >/dev/null 2>&1; then
    echo "judge-api exited early" >&2
    cat "$evidence_dir/logs/judge-api.log" >&2 || true
    exit 1
  fi
  sleep 1
done
curl -fsS "http://127.0.0.1:$judge_api_port/health" >"$evidence_dir/judge-api-health.json"
curl -fsS \
  -H 'X-Auth-Verified: true' \
  -H 'X-User-Id: 1' \
  -H 'X-Username: redis-drill-admin' \
  -H 'X-Roles: admin' \
  "http://127.0.0.1:$judge_api_port/judge/admin/queue/status" >"$evidence_dir/queue-status.json"

jq -e \
  --arg stream "$stream" \
  --arg result_stream "$result_stream" \
  --arg group "$group" \
  '.task_stream == $stream
    and .result_stream == $result_stream
    and .group == $group
    and .pending_count == 0
    and (.redis_status == "ok" or .redis_status == "partial")
    and .consumer_count > 0
    and (.consumers | length) > 0
    and (.last_id | length) > 0
    and (.result_last_id | length) > 0' \
  "$evidence_dir/queue-status.json" >/dev/null
queue_status_api="verified by judge-api /judge/admin/queue/status"
