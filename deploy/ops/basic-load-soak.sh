#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_LOAD_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/basic-load-soak/$run_id}"
mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd)"
mkdir -p "$evidence_dir/logs" "$evidence_dir/responses"
log_file="$evidence_dir/logs/basic-load-soak.log"
exec > >(tee -a "$log_file") 2>&1
export NO_PROXY="${NO_PROXY:-localhost,127.0.0.1,::1},localhost,127.0.0.1,::1"
export no_proxy="${no_proxy:-$NO_PROXY}"

compose_file="$repo_root/deploy/compose/docker-compose.yml"
compose_dev_file="${OJOS_COMPOSE_DEV_OVERRIDE:-$repo_root/deploy/compose/docker-compose.dev.yml}"
env_file="${OJOS_COMPOSE_ENV_FILE:-$repo_root/.env.example}"
bootstrap_compose="${OJOS_LOAD_DRILL_BOOTSTRAP_COMPOSE:-0}"
build_compose="${OJOS_LOAD_DRILL_BUILD_COMPOSE:-0}"
cleanup_compose="${OJOS_LOAD_DRILL_CLEANUP_COMPOSE:-0}"
run_migrations="${OJOS_LOAD_DRILL_RUN_MIGRATIONS:-1}"
run_smoke="${OJOS_LOAD_DRILL_RUN_SMOKE:-1}"
concurrency="${OJOS_LOAD_CONCURRENCY:-20}"
request_count="${OJOS_LOAD_REQUESTS:-40}"
min_success_rate="${OJOS_LOAD_MIN_SUCCESS_RATE:-0.95}"
# Opt-in p95 latency ceiling in milliseconds. Empty by default so the smoke keeps its
# single success-rate gate; set it (with raised concurrency/requests) to start turning
# this smoke into a capacity baseline.
max_p95_ms="${OJOS_LOAD_MAX_P95_MS:-}"
redis_password="${REDIS_PASSWORD:-DEV_ONLY_redis_password_not_for_production}"
redis_url="${OJOS_LOAD_DRILL_REDIS_URL:-${REDIS_URL:-redis://:$redis_password@127.0.0.1:${REDIS_HOST_PORT:-6379}/0}}"
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
submission_id="${OJOS_LOAD_SUBMISSION_ID:-}"
problem_id="${OJOS_LOAD_PROBLEM_ID:-}"
worker_processed_count="0"
queue_pending_max="0"
success_rate="0"
p95_ms="0"
error_count="0"
queue_admin_token="${OJOS_LOAD_QUEUE_ADMIN_TOKEN:-}"

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
最小修复建议：安装或启动缺失依赖后重跑 basic load/soak drill
后续处理：需要用户介入
OJOS_ENV_BLOCKED
  exit 127
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || env_blocked "basic-load-soak" "command -v $1" "$1 not found"
}

resolve_python_bin() {
  if [[ -n "${PYTHON_BIN:-}" ]]; then
    command -v "$PYTHON_BIN" >/dev/null 2>&1 || env_blocked "basic-load-soak" "command -v $PYTHON_BIN" "$PYTHON_BIN not found"
    printf '%s\n' "$PYTHON_BIN"
    return
  fi
  if command -v python3 >/dev/null 2>&1; then
    printf '%s\n' "python3"
    return
  fi
  if command -v python >/dev/null 2>&1; then
    printf '%s\n' "python"
    return
  fi
  env_blocked "basic-load-soak" "command -v python3 || command -v python" "python interpreter not found"
}

docker_cli_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

compose_file_for_docker="$(docker_cli_path "$compose_file")"
compose_dev_file_for_docker="$(docker_cli_path "$compose_dev_file")"
env_file_for_docker="$(docker_cli_path "$env_file")"

docker_compose() {
  MSYS2_ARG_CONV_EXCL='*' docker compose --profile legacy-development --env-file "$env_file_for_docker" \
    -f "$compose_file_for_docker" -f "$compose_dev_file_for_docker" "$@"
}

run_compose_migrations() {
  for migration_service in \
    auth-service-migrations \
    problem-service-migrations \
    judge-api-migrations \
    user-service-migrations
  do
    docker_compose run --rm "$migration_service"
  done
}

redis_xlen() {
  docker_compose exec -T redis redis-cli -a "$redis_password" XLEN ojos:judge:result | tr -d '\r[:space:]'
}

login_queue_admin() {
  local response token
  response="$(curl -fsS \
    -H 'Content-Type: application/json' \
    --data-binary @- \
    'http://127.0.0.1:8081/auth/login' <<'OJOS_QUEUE_ADMIN_LOGIN'
{"username":"compose-smoke-admin","password":"compose-smoke-admin-password"}
OJOS_QUEUE_ADMIN_LOGIN
)"
  token="$(jq -er '.data.token | select(type == "string" and length > 0)' <<<"$response")"
  printf '%s\n' "$token"
}

validate_queue_admin_token() {
  local token="$1"
  if [[ ! "$token" =~ ^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$ ]]; then
    echo "compose queue administrator login returned an invalid JWT" >&2
    return 1
  fi
}

queue_status() {
  local out="$1"
  curl -fsS --config - >"$out" <<OJOS_QUEUE_STATUS_CURL
url = "http://127.0.0.1:8080/api/judge/admin/queue/status"
header = "Authorization: Bearer $queue_admin_token"
header = "X-OJOS-Node-Id: child-node"
OJOS_QUEUE_STATUS_CURL
}

validate_load_gate() {
  local metrics_file="$1"
  local queue_after_file="$2"
  local required_min_success_rate="$3"

  jq -e --arg min_success_rate "$required_min_success_rate" '
    ((.success_rate | type) == "number")
    and (.success_rate >= ($min_success_rate | tonumber))
    and (.by_operation["judge-submit"].total == 1)
    and (.by_operation["judge-submit"].ok == 1)
    and ((.worker_processed_count | type) == "number")
    and (.worker_processed_count >= 1)
  ' "$metrics_file" >/dev/null
  jq -e '.pending_count == 0' "$queue_after_file" >/dev/null
}

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker_compose ps -a >"$evidence_dir/logs/compose-ps.txt" 2>&1 || true
  docker_compose logs --no-color \
    auth-service gateway problem-service storage-service \
    judge-api judge-worker jaeger redis minio \
    >"$evidence_dir/logs/compose-services.log" 2>&1 || true
  if [[ "$bootstrap_compose" == "1" && "$cleanup_compose" == "1" ]]; then
    docker_compose down --remove-orphans >/dev/null 2>&1 || true
  fi
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg concurrency "$concurrency" \
    --arg request_count "$request_count" \
    --arg success_rate "$success_rate" \
    --arg p95_ms "$p95_ms" \
    --arg error_count "$error_count" \
    --arg queue_pending_max "$queue_pending_max" \
    --arg worker_processed_count "$worker_processed_count" \
    --arg submission_id "$submission_id" \
    --arg problem_id "$problem_id" \
    '{
      drill: "basic-load-soak-smoke",
      status: $status,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      concurrency: ($concurrency | tonumber),
      request_count: ($request_count | tonumber),
      success_rate: ($success_rate | tonumber),
      p95_ms: ($p95_ms | tonumber),
      error_count: ($error_count | tonumber),
      queue_pending_max: ($queue_pending_max | tonumber),
      worker_processed_count: ($worker_processed_count | tonumber),
      seeded_submission_id: $submission_id,
      seeded_problem_id: $problem_id,
      scope: "basic load/soak smoke only; not a capacity test",
      evidence: {
        log: "logs/basic-load-soak.log",
        smoke_log: "logs/judge-local-smoke.log",
        results: "responses/results.jsonl",
        metrics: "responses/metrics.json",
        queue_before: "responses/queue-before.json",
        queue_after: "responses/queue-after.json",
        compose_logs: "logs/compose-services.log",
        compose_ps: "logs/compose-ps.txt"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] basic load/soak smoke has metrics and threshold"
  else
    echo "[FAILED] basic load/soak drill failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

need_cmd docker
need_cmd go
need_cmd jq
need_cmd curl
need_cmd sed
python_bin="$(resolve_python_bin)"

if [[ "$run_migrations" == "1" ]]; then
  run_compose_migrations
fi

if [[ "$bootstrap_compose" == "1" ]]; then
  compose_up_args=(up -d)
  if [[ "$build_compose" == "1" ]]; then
    compose_up_args+=(--build)
  fi
  compose_up_args+=(
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

if [[ "$run_smoke" == "1" ]]; then
  (
    cd "$repo_root/services/judge-api"
    OJOS_SMOKE_STORAGE_BACKEND=minio \
    go run ./cmd/judge-local-smoke -mode compose -storage-backend minio -redis "$redis_url" -timeout 420s
  ) >"$evidence_dir/logs/judge-local-smoke.log" 2>&1
  submission_id="$(sed -n 's/.*submission_id=\([0-9][0-9]*\).*/\1/p' "$evidence_dir/logs/judge-local-smoke.log" | tail -n 1)"
  problem_id="$(sed -n 's/.*problem_id=\([0-9][0-9]*\).*/\1/p' "$evidence_dir/logs/judge-local-smoke.log" | tail -n 1)"
fi

if [[ -z "$submission_id" || -z "$problem_id" ]]; then
  echo "submission_id and problem_id are required; run smoke or set OJOS_LOAD_SUBMISSION_ID/OJOS_LOAD_PROBLEM_ID" >&2
  exit 1
fi

if [[ -z "$queue_admin_token" ]]; then
  queue_admin_token="$(login_queue_admin)"
fi
validate_queue_admin_token "$queue_admin_token"

queue_status "$evidence_dir/responses/queue-before.json"
result_len_before="$(redis_xlen)"

LOAD_EVIDENCE_DIR="$evidence_dir" \
LOAD_RUN_ID="$run_id" \
LOAD_CONCURRENCY="$concurrency" \
LOAD_REQUEST_COUNT="$request_count" \
LOAD_PROBLEM_ID="$problem_id" \
LOAD_SUBMISSION_ID="$submission_id" \
"$python_bin" - <<'PY'
import concurrent.futures
import json
import os
import statistics
import time
import urllib.error
import urllib.request

evidence_dir = os.environ["LOAD_EVIDENCE_DIR"]
run_id = os.environ["LOAD_RUN_ID"]
concurrency = int(os.environ["LOAD_CONCURRENCY"])
request_count = max(int(os.environ["LOAD_REQUEST_COUNT"]), concurrency)
problem_id = int(os.environ["LOAD_PROBLEM_ID"])
submission_id = int(os.environ["LOAD_SUBMISSION_ID"])
results_path = os.path.join(evidence_dir, "responses", "results.jsonl")

auth_base = "http://127.0.0.1:8081"
gateway_base = "http://127.0.0.1:8080"
storage_base = "http://127.0.0.1:8085"

def http_call(method, url, body=None, headers=None, timeout=30):
    data = None
    req_headers = dict(headers or {})
    if body is not None:
        data = json.dumps(body).encode("utf-8") if not isinstance(body, (bytes, bytearray)) else body
        req_headers.setdefault("Content-Type", "application/json")
    req = urllib.request.Request(url, data=data, headers=req_headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            payload = resp.read()
            return resp.status, payload
    except urllib.error.HTTPError as err:
        return err.code, err.read()

def login():
    status, body = http_call("POST", auth_base + "/auth/login", {
        "username": "compose-smoke",
        "password": "compose-smoke-password",
    })
    if status // 100 != 2:
        raise RuntimeError(f"login status {status}: {body[:120]!r}")
    data = json.loads(body.decode("utf-8"))
    token = data.get("data", {}).get("token", "")
    if not token:
        raise RuntimeError(f"login returned no token: {data!r}")
    return token

token = login()
auth_headers = {"Authorization": "Bearer " + token}

def timed(name, func):
    started = time.perf_counter()
    status = 0
    error = ""
    ok = False
    try:
        status = func()
        ok = 200 <= int(status) < 300
    except Exception as exc:
        error = str(exc)
    elapsed_ms = (time.perf_counter() - started) * 1000
    return {
        "operation": name,
        "ok": ok,
        "status": int(status),
        "latency_ms": round(elapsed_ms, 3),
        "error": error,
    }

def op_auth_login(i):
    del i
    status, _ = http_call("POST", auth_base + "/auth/login", {
        "username": "compose-smoke",
        "password": "compose-smoke-password",
    })
    return status

def op_problem_list(i):
    del i
    status, _ = http_call("GET", gateway_base + "/api/problem/problems?page=1&page_size=20", headers=auth_headers)
    return status

def op_storage_put_get(i):
    key = f"load-{run_id}-{i}.txt"
    body = f"load drill {run_id} {i}\n".encode("utf-8")
    put_status, _ = http_call(
        "PUT",
        storage_base + "/api/storage/objects/judge-artifacts/" + key,
        body=body,
        headers={"Content-Type": "text/plain; charset=utf-8"},
    )
    if put_status // 100 != 2:
        return put_status
    get_status, get_body = http_call(
        "GET",
        storage_base + "/api/storage/objects/judge-artifacts/" + key,
    )
    if get_status // 100 == 2 and get_body != body:
        raise RuntimeError("storage body mismatch")
    return get_status

def op_result_query(i):
    del i
    status, _ = http_call("GET", gateway_base + f"/api/judge/submissions/{submission_id}", headers=auth_headers)
    return status

def op_judge_submit(i):
    del i
    source = '#include <iostream>\nint main(){ std::cout << "ok\\n"; return 0; }\n'
    status, body = http_call("POST", gateway_base + "/api/judge/submissions", {
        "problem_id": problem_id,
        "language": "cpp17",
        "code": source,
    }, headers=auth_headers, timeout=60)
    if status // 100 != 2:
        return status
    data = json.loads(body.decode("utf-8"))
    new_submission_id = int(data.get("submission_id") or data.get("data", {}).get("submission_id") or 0)
    if new_submission_id <= 0:
        raise RuntimeError(f"judge submit returned no submission id: {data!r}")
    deadline = time.time() + 90
    last_status = ""
    while time.time() < deadline:
        get_status, get_body = http_call("GET", gateway_base + f"/api/judge/submissions/{new_submission_id}", headers=auth_headers, timeout=30)
        if get_status // 100 != 2:
            return get_status
        payload = json.loads(get_body.decode("utf-8"))
        last_status = str(payload.get("status") or payload.get("data", {}).get("status") or "")
        if last_status.upper() == "ACCEPTED":
            return get_status
        time.sleep(1)
    raise RuntimeError(f"judge submission did not finish; last_status={last_status}")

ops = [
    ("auth-login", op_auth_login),
    ("problem-list", op_problem_list),
    ("storage-put-get", op_storage_put_get),
    ("result-query", op_result_query),
]
work = []
for i in range(request_count):
    name, fn = ops[i % len(ops)]
    work.append((name, fn, i))
work.append(("judge-submit", op_judge_submit, request_count + 1))

with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
    futures = [pool.submit(timed, name, lambda fn=fn, i=i: fn(i)) for name, fn, i in work]
    results = [future.result() for future in concurrent.futures.as_completed(futures)]

with open(results_path, "w", encoding="utf-8") as fh:
    for result in results:
        fh.write(json.dumps(result, sort_keys=True) + "\n")

latencies = sorted(result["latency_ms"] for result in results)
successes = sum(1 for result in results if result["ok"])
errors = len(results) - successes
p50 = statistics.median(latencies) if latencies else 0.0
p95 = latencies[max(0, int(len(latencies) * 0.95 + 0.999) - 1)] if latencies else 0.0
by_operation = {}
for result in results:
    item = by_operation.setdefault(result["operation"], {"total": 0, "ok": 0})
    item["total"] += 1
    if result["ok"]:
        item["ok"] += 1

metrics = {
    "scope": "basic load/soak smoke only; not a capacity test",
    "concurrency": concurrency,
    "request_count": len(results),
    "success_rate": round(successes / len(results), 6) if results else 0,
    "error_count": errors,
    "p50_ms": round(p50, 3),
    "p95_ms": round(p95, 3),
    "by_operation": by_operation,
}
with open(os.path.join(evidence_dir, "responses", "metrics.json"), "w", encoding="utf-8") as fh:
    json.dump(metrics, fh, indent=2, sort_keys=True)
PY

queue_status "$evidence_dir/responses/queue-after.json"
result_len_after="$(redis_xlen)"
worker_processed_count="$((result_len_after - result_len_before))"
if [[ "$worker_processed_count" -lt 0 ]]; then
  worker_processed_count="0"
fi
queue_pending_max="$(jq -s 'map(.pending_count // 0) | max' "$evidence_dir/responses/queue-before.json" "$evidence_dir/responses/queue-after.json")"

jq \
  --arg queue_pending_max "$queue_pending_max" \
  --arg worker_processed_count "$worker_processed_count" \
  --arg min_success_rate "$min_success_rate" \
  --arg max_p95_ms "$max_p95_ms" \
  '. + {
    queue_pending_max: ($queue_pending_max | tonumber),
    worker_processed_count: ($worker_processed_count | tonumber),
    threshold: {
      min_success_rate: ($min_success_rate | tonumber),
      max_p95_ms: (if $max_p95_ms == "" then null else ($max_p95_ms | tonumber) end)
    }
  }' "$evidence_dir/responses/metrics.json" >"$evidence_dir/responses/metrics.tmp.json"
mv "$evidence_dir/responses/metrics.tmp.json" "$evidence_dir/responses/metrics.json"

success_rate="$(jq -r '.success_rate' "$evidence_dir/responses/metrics.json")"
p95_ms="$(jq -r '.p95_ms' "$evidence_dir/responses/metrics.json")"
error_count="$(jq -r '.error_count' "$evidence_dir/responses/metrics.json")"

validate_load_gate \
  "$evidence_dir/responses/metrics.json" \
  "$evidence_dir/responses/queue-after.json" \
  "$min_success_rate"

# Opt-in p95 latency ceiling: only enforced when OJOS_LOAD_MAX_P95_MS is set.
if [[ -n "$max_p95_ms" ]]; then
  jq -e --arg max_p95_ms "$max_p95_ms" \
    '.p95_ms <= ($max_p95_ms | tonumber)' \
    "$evidence_dir/responses/metrics.json" >/dev/null
fi
