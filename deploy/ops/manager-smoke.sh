#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_MANAGER_SMOKE_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/manager-smoke/$run_id}"
mkdir -p "$evidence_dir/logs" "$evidence_dir/responses"
log_file="$evidence_dir/logs/manager-smoke.log"
exec > >(tee -a "$log_file") 2>&1

port="${OJOS_MANAGER_SMOKE_ORCHESTRATOR_PORT:-18091}"
base_url="http://127.0.0.1:$port"
daemon_pid=""
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{
      drill: "manager-gui-tui-operator-smoke",
      status: $status,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      manager_auth: "deferred",
      manager_mode: "read-only/dev-ops beta",
      evidence: {
        log: "logs/manager-smoke.log",
        daemon: "logs/orchestrator-daemon.log",
        responses: "responses/"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] manager GUI/TUI have minimum operator smoke"
  else
    echo "[FAILED] manager smoke failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

command -v cargo >/dev/null 2>&1 || { echo "[ENV-BLOCKED] cargo" >&2; exit 127; }
command -v curl >/dev/null 2>&1 || { echo "[ENV-BLOCKED] curl" >&2; exit 127; }
command -v jq >/dev/null 2>&1 || { echo "[ENV-BLOCKED] jq" >&2; exit 127; }

(
  cd "$repo_root"
  cargo run -q -p ojos-orchestrator-daemon -- --repo-root "$repo_root" --bind "127.0.0.1:$port"
) >"$evidence_dir/logs/orchestrator-daemon.log" 2>&1 &
daemon_pid="$!"

for _ in $(seq 1 120); do
  if curl -fsS "$base_url/health" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$daemon_pid" >/dev/null 2>&1; then
    cat "$evidence_dir/logs/orchestrator-daemon.log" >&2 || true
    exit 1
  fi
  sleep 1
done

curl -fsS "$base_url/health" >"$evidence_dir/responses/health.json"
curl -fsS "$base_url/services" >"$evidence_dir/responses/services.json"
curl -fsS "$base_url/endpoints" >"$evidence_dir/responses/endpoints.json"
curl -fsS "$base_url/operations" >"$evidence_dir/responses/operations.json"

jq -e '.services | length > 0' "$evidence_dir/responses/services.json" >/dev/null
jq -e '.endpoints | length > 0' "$evidence_dir/responses/endpoints.json" >/dev/null
jq -e '.operations | length > 0' "$evidence_dir/responses/operations.json" >/dev/null

operation_id="$(jq -r '.operations[0].operation_id' "$evidence_dir/responses/operations.json")"
curl -fsS "$base_url/operations/$operation_id" >"$evidence_dir/responses/operation-detail.json"
curl -fsS "$base_url/operations/$operation_id/logs" >"$evidence_dir/responses/operation-logs.json"
jq -e '.operation.operation_id != ""' "$evidence_dir/responses/operation-detail.json" >/dev/null
jq -e '.logs != null' "$evidence_dir/responses/operation-logs.json" >/dev/null

if curl -fsS --max-time 2 "http://127.0.0.1:1/health" >"$evidence_dir/responses/bad-endpoint.txt" 2>&1; then
  echo "bad endpoint unexpectedly succeeded" >&2
  exit 1
else
  echo "bad endpoint reported error as expected" >"$evidence_dir/responses/bad-endpoint.txt"
fi

cargo test -p ojos-orchestrator-gui gui_loads_shared_operation_workbench_from_core
cargo test -p ojos-orchestrator-tui tui_loads_shared_orchestrator_view_from_core
