#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_TRACE_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/trace-e2e-drill/$run_id}"
mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd)"
mkdir -p "$evidence_dir/logs" "$evidence_dir/responses"
log_file="$evidence_dir/logs/trace-e2e-drill.log"
exec > >(tee -a "$log_file") 2>&1
export NO_PROXY="${NO_PROXY:-localhost,127.0.0.1,::1},localhost,127.0.0.1,::1"
export no_proxy="${no_proxy:-$NO_PROXY}"

compose_file="$repo_root/deploy/compose/docker-compose.yml"
env_file="${OJOS_COMPOSE_ENV_FILE:-$repo_root/.env.example}"
bootstrap_compose="${OJOS_TRACE_DRILL_BOOTSTRAP_COMPOSE:-0}"
build_compose="${OJOS_TRACE_DRILL_BUILD_COMPOSE:-0}"
cleanup_compose="${OJOS_TRACE_DRILL_CLEANUP_COMPOSE:-0}"
run_migrations="${OJOS_TRACE_DRILL_RUN_MIGRATIONS:-1}"
jaeger_url="${OJOS_JAEGER_QUERY_URL:-http://127.0.0.1:16686}"
redis_password="${REDIS_PASSWORD:-DEV_ONLY_redis_password_not_for_production}"
redis_url="${REDIS_URL:-redis://:$redis_password@127.0.0.1:6379/0}"
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
trace_id=""
submission_id=""
task_entry_id=""
traceparent=""
service_list_json="[]"
span_count="0"

env_blocked() {
  local component="$1"
  local command="$2"
  local summary="$3"
  cat >&2 <<OJOS_ENV_BLOCKED
[ENV-BLOCKED] $component
命令：$command
错误摘要：$summary
判断：环境问题
是否阻塞当前任务：是
最小修复建议：安装或启动缺失依赖后重跑 trace E2E drill
后续处理：需要用户介入
OJOS_ENV_BLOCKED
  exit 127
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || env_blocked "trace-e2e" "command -v $1" "$1 not found"
}

docker_cli_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

compose_file_for_docker="$(docker_cli_path "$compose_file")"
env_file_for_docker="$(docker_cli_path "$env_file")"

docker_compose() {
  MSYS2_ARG_CONV_EXCL='*' docker compose --env-file "$env_file_for_docker" -f "$compose_file_for_docker" "$@"
}

run_compose_migrations() {
  for migration_service in \
    orchestrator-migrations \
    auth-service-migrations \
    problem-service-migrations \
    judge-api-migrations \
    user-service-migrations
  do
    docker_compose run --rm "$migration_service"
  done
}

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker_compose logs --no-color jaeger gateway judge-api storage-service judge-worker >"$evidence_dir/logs/compose-services.log" 2>&1 || true
  if [[ "$bootstrap_compose" == "1" && "$cleanup_compose" == "1" ]]; then
    docker_compose down --remove-orphans >/dev/null 2>&1 || true
  fi
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg run_id "$run_id" \
    --arg submission_id "$submission_id" \
    --arg task_entry_id "$task_entry_id" \
    --arg traceparent "$traceparent" \
    --arg trace_id "$trace_id" \
    --argjson service_list "$service_list_json" \
    --arg span_count "$span_count" \
    '{
      drill: "judge-submission-trace-e2e",
      status: $status,
      run_id: $run_id,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      submission_id: $submission_id,
      task_entry_id: $task_entry_id,
      traceparent: $traceparent,
      trace_id: $trace_id,
      service_list: $service_list,
      span_count: ($span_count | tonumber),
      redis_worker_boundary: "metadata-linked: traceparent is persisted in Redis task event; judge-worker extracts it and exports a native OTLP consumer span",
      evidence: {
        log: "logs/trace-e2e-drill.log",
        smoke_log: "logs/judge-local-smoke.log",
        redis_task_entry: "responses/redis-task-entry.txt",
        jaeger_trace: "responses/jaeger-trace.json",
        trace_summary: "responses/trace-summary.json",
        compose_logs: "logs/compose-services.log"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] judge submission E2E trace visible in Jaeger"
  else
    echo "[FAILED] trace E2E drill failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

need_cmd docker
need_cmd go
need_cmd jq
need_cmd sed
need_cmd grep
need_cmd curl

if [[ "$run_migrations" == "1" ]]; then
  run_compose_migrations
fi

if [[ "$bootstrap_compose" == "1" ]]; then
  compose_up_args=(up -d)
  if [[ "$build_compose" == "1" ]]; then
    compose_up_args+=(--build)
  fi
  compose_up_args+=(
    orchestrator
    auth-service
    storage-service
    gateway
    problem-service
    judge-api
    judge-worker
    jaeger
    redis
    minio
  )
  docker_compose "${compose_up_args[@]}"
fi

(
  cd "$repo_root/services/judge-api"
  OJOS_SMOKE_STORAGE_BACKEND=minio \
  go run ./cmd/judge-local-smoke -mode compose -storage-backend minio -redis "$redis_url" -timeout 420s
) >"$evidence_dir/logs/judge-local-smoke.log" 2>&1

submission_id="$(sed -n 's/.*submission_id=\([0-9][0-9]*\).*/\1/p' "$evidence_dir/logs/judge-local-smoke.log" | tail -n 1)"
task_entry_id="$(sed -n 's/.*task_entry_id=\([^ ]*\).*/\1/p' "$evidence_dir/logs/judge-local-smoke.log" | tail -n 1)"
if [[ -z "$submission_id" || -z "$task_entry_id" ]]; then
  echo "could not parse submission_id/task_entry_id from smoke log" >&2
  exit 1
fi

docker_compose exec -T redis redis-cli -a "$redis_password" XRANGE ojos:judge:task "$task_entry_id" "$task_entry_id" \
  >"$evidence_dir/responses/redis-task-entry.txt"
traceparent="$(grep -Eo '00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}' "$evidence_dir/responses/redis-task-entry.txt" | head -n 1 || true)"
if [[ -z "$traceparent" ]]; then
  echo "Redis task entry did not include traceparent" >&2
  exit 1
fi
trace_id="${traceparent#00-}"
trace_id="${trace_id%%-*}"

jaeger_get() {
  local path="$1"
  local out="$2"
  if curl -fsS "$jaeger_url$path" >"$out" 2>"$out.err"; then
    rm -f "$out.err"
    return 0
  fi
  docker_compose exec -T jaeger wget -qO- "http://127.0.0.1:16686$path" >"$out"
}

trace_has_required_services() {
  local trace_file="$1"
  jq -e '
    (.data | length > 0)
    and ((.data[0].processes | to_entries | map(.value.serviceName) | unique) as $services
      | ($services | index("gateway-service"))
        and ($services | index("judge-api-service"))
        and ($services | index("storage-service"))
        and ($services | index("judge-worker")))
    and ((.data[0].spans | map(.operationName)) | index("judge-worker execute task"))
  ' "$trace_file" >/dev/null 2>&1
}

for _ in $(seq 1 60); do
  jaeger_get "/api/traces/$trace_id" "$evidence_dir/responses/jaeger-trace.json" || true
  if trace_has_required_services "$evidence_dir/responses/jaeger-trace.json"; then
    break
  fi
  sleep 2
done
jq -e '.data | length > 0' "$evidence_dir/responses/jaeger-trace.json" >/dev/null

trace_has_required_services "$evidence_dir/responses/jaeger-trace.json"

jq '{
  trace_id: .data[0].traceID,
  service_list: (.data[0].processes | to_entries | map(.value.serviceName) | unique),
  span_count: (.data[0].spans | length),
  operations: (.data[0].spans | map(.operationName) | unique),
  redis_worker_boundary: "metadata-linked-with-worker-native-span"
}' "$evidence_dir/responses/jaeger-trace.json" >"$evidence_dir/responses/trace-summary.json"

service_list_json="$(jq -c '.service_list' "$evidence_dir/responses/trace-summary.json")"
span_count="$(jq -r '.span_count' "$evidence_dir/responses/trace-summary.json")"
