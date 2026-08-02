#!/usr/bin/env bash
set -euo pipefail

load_env_file() {
  local env_file="${OJOS_ENV_FILE:-}"
  if [[ -n "$env_file" ]]; then
    if [[ ! -f "$env_file" ]]; then
      echo "rollback-drill: OJOS_ENV_FILE does not exist: $env_file" >&2
      exit 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
      line="${line%$'\r'}"
      [[ "$line" =~ ^[[:space:]]*$ || "$line" =~ ^[[:space:]]*# ]] && continue
      line="${line#export }"
      key="${line%%=*}"
      value="${line#*=}"
      key="$(printf '%s' "$key" | xargs)"
      [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
      if [[ "$value" =~ ^\".*\"$ || "$value" =~ ^\'.*\'$ ]]; then
        value="${value:1:${#value}-2}"
      fi
      export "$key=$value"
    done <"$env_file"
  fi
}

die() {
  echo "rollback-drill: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

curl_json() {
  local method="$1"
  local url="$2"
  local body="${3:-}"
  local args=(-fsS -X "$method" -H "Content-Type: application/json")
  if [[ -n "${ORCHESTRATOR_INTERNAL_TOKEN:-}" ]]; then
    args+=(-H "x-ojos-orchestrator-token: $ORCHESTRATOR_INTERNAL_TOKEN")
  fi
  if [[ -n "${ORCHESTRATOR_TOKEN:-}" ]]; then
    args+=(-H "Authorization: Bearer $ORCHESTRATOR_TOKEN")
  fi
  if [[ -n "$body" ]]; then
    args+=(-d "$body")
  fi
  curl "${args[@]}" "$url"
}

load_env_file
need_cmd curl
need_cmd jq

base_url="${ORCHESTRATOR_URL:-${ORCHESTRATOR_ENDPOINT:-}}"
[[ -n "$base_url" ]] || die "ORCHESTRATOR_URL or ORCHESTRATOR_ENDPOINT is required"
base_url="${base_url%/}"

target_operation="${OJOS_ROLLBACK_OPERATION_ID:-}"
release_service="${OJOS_ROLLBACK_SERVICE:-}"
release_version="${OJOS_ROLLBACK_RELEASE_VERSION:-}"

if [[ -z "$target_operation" && -z "$release_service" ]]; then
  die "set OJOS_ROLLBACK_OPERATION_ID for operation rollback or OJOS_ROLLBACK_SERVICE for release rollback"
fi
if [[ -n "$target_operation" && -n "$release_service" ]]; then
  die "OJOS_ROLLBACK_OPERATION_ID and OJOS_ROLLBACK_SERVICE are mutually exclusive"
fi
if [[ -n "$release_service" && "${OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER:-0}" != "1" ]]; then
  die "release rollback requires OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER=1"
fi

confirm_target="${target_operation:-$release_service}"
expected_confirm="rollback-$confirm_target"
[[ "${OJOS_CONFIRM_ROLLBACK:-}" == "$expected_confirm" ]] || die "set OJOS_CONFIRM_ROLLBACK=$expected_confirm to execute the rollback drill"

if [[ -n "$target_operation" ]]; then
  echo "rollback-drill: rolling back operation $target_operation"
  execute_driver="${OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER:-0}"
  body="$(jq -cn \
    --arg target "$target_operation" \
    --arg execute "$execute_driver" \
    '{
      action: "operation.rollback",
      fields: (
        {operation_id: $target, confirm: "true"}
        + (if $execute == "1" then {execute_service_driver: "true"} else {} end)
      )
    }')"
  response="$(curl_json POST "$base_url/actions" "$body")"
  status="$(printf '%s' "$response" | jq -r '.action_result.status // .operation.status // empty')"
  [[ "$status" == "ROLLED_BACK" || "$status" == "SUCCEEDED" ]] || die "unexpected rollback status: $status"
  after="$(curl_json GET "$base_url/operations/$target_operation")"
  after_status="$(printf '%s' "$after" | jq -r '.operation.status // empty')"
  [[ "$after_status" == "ROLLED_BACK" ]] || die "operation did not end as ROLLED_BACK: $after_status"
  echo "rollback-drill: operation $target_operation reached ROLLED_BACK"
else
  path="$base_url/releases/$release_service"
  if [[ -n "$release_version" ]]; then
    path="$path/$release_version"
  fi
  path="$path/rollback"
  body="$(jq -cn \
    --arg target "${OJOS_ROLLBACK_TARGET_OPERATION_ID:-}" \
    --arg execute "${OJOS_ROLLBACK_EXECUTE_SERVICE_DRIVER:-0}" \
    '{
      fields: (
        {}
        + (if $target != "" then {target_operation_id: $target} else {} end)
        + (if $execute == "1" then {execute_service_driver: "true"} else {} end)
      )
    }')"
  echo "rollback-drill: rolling back release $release_service"
  response="$(curl_json POST "$path" "$body")"
  status="$(printf '%s' "$response" | jq -r '.action_result.status // empty')"
  [[ "$status" == "SUCCEEDED" || "$status" == "ROLLED_BACK" ]] || die "unexpected release rollback status: $status"
  echo "rollback-drill: release rollback accepted with status $status"
fi
