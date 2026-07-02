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
redis_image="${OJOS_DRILL_REDIS_IMAGE:-redis:8.8.0}"
stream="ojos:judge:task:drill:$run_id"
result_stream="ojos:judge:result:drill:$run_id"
group="ojos-judge-workers"
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

docker_exec() {
  MSYS2_ARG_CONV_EXCL='*' docker exec "$@"
}

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker logs "$container" >"$evidence_dir/logs/redis.log" 2>&1 || true
  docker rm -f "$container" >/dev/null 2>&1 || true
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg stream "$stream" \
    --arg result_stream "$result_stream" \
    '{
      drill: "redis-pending-recovery",
      status: $status,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      task_stream: $stream,
      result_stream: $result_stream,
      persistence: "appendonly yes verified across docker restart",
      queue_status_api: "deferred to judge-api compose smoke; this drill verifies Redis stream recovery primitive",
      evidence: {
        log: "logs/redis-recovery-drill.log",
        result: "result.json"
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

docker run -d --name "$container" "$redis_image" redis-server --appendonly yes >/dev/null
for _ in $(seq 1 60); do
  if docker_exec "$container" redis-cli ping >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker_exec "$container" redis-cli ping >/dev/null

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
