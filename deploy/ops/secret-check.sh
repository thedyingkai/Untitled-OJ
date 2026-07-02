#!/usr/bin/env bash
set -euo pipefail

load_env_file() {
  local env_file="${OJOS_ENV_FILE:-}"
  if [[ -n "$env_file" ]]; then
    if [[ ! -f "$env_file" ]]; then
      echo "secret-check: OJOS_ENV_FILE does not exist: $env_file" >&2
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
  echo "secret-check: $*" >&2
  exit 1
}

value_for() {
  local name="$1"
  local file_name="${name}_FILE"
  local value="${!name:-}"
  local file_value="${!file_name:-}"
  if [[ -z "$value" && -n "$file_value" ]]; then
    [[ -f "$file_value" ]] || die "$file_name points to a missing file: $file_value"
    value="$(tr -d '\r\n' <"$file_value")"
  fi
  printf '%s' "$value"
}

reject_weak_value() {
  local name="$1"
  local value="$2"
  local lower
  lower="$(printf '%s' "$value" | tr '[:upper:]' '[:lower:]')"
  [[ "$value" != *"<"* && "$value" != *">"* ]] || die "$name still contains placeholder brackets"
  case "$lower" in
    ""|changeme|change-me|default|example|password|secret|token|admin|postgres|root)
      die "$name uses a forbidden placeholder/default value"
      ;;
  esac
  if [[ "$lower" =~ (dev_only|ojos-local|static-compose|smoke|test-token|worker-token|jwt-secret|internal-token|minio-password|minio-user|local-worker|local-jwt|local-internal) ]]; then
    die "$name uses a known non-production value"
  fi
}

require_secret() {
  local name="$1"
  local min_len="$2"
  local value
  value="$(value_for "$name")"
  [[ -n "$value" ]] || die "$name is required"
  reject_weak_value "$name" "$value"
  if (( ${#value} < min_len )); then
    die "$name must be at least $min_len characters"
  fi
}

require_database_url() {
  local name="$1"
  local value
  value="$(value_for "$name")"
  [[ -n "$value" ]] || die "$name is required"
  reject_weak_value "$name" "$value"
  [[ "$value" =~ ^postgres(ql)?://[^:/@]+:[^@]+@[^/]+/.+ ]] || die "$name must be a password-authenticated PostgreSQL URL"
  if [[ "$value" =~ ^postgres(ql)?://postgres: ]]; then
    die "$name must not use the postgres superuser"
  fi
  if [[ "$value" =~ (127\.0\.0\.1|localhost) && "${OJOS_SECRET_CHECK_ALLOW_LOCAL:-0}" != "1" ]]; then
    die "$name points at localhost; set OJOS_SECRET_CHECK_ALLOW_LOCAL=1 only for non-production drills"
  fi
}

require_redis_url() {
  local name="$1"
  local value
  value="$(value_for "$name")"
  [[ -n "$value" ]] || die "$name is required"
  reject_weak_value "$name" "$value"
  [[ "$value" =~ ^rediss?://([^:/@]+:)?[^@]+@[^/]+(/[0-9]+)?$ ]] || die "$name must be a password-authenticated Redis URL"
  if [[ "$value" =~ (127\.0\.0\.1|localhost) && "${OJOS_SECRET_CHECK_ALLOW_LOCAL:-0}" != "1" ]]; then
    die "$name points at localhost; set OJOS_SECRET_CHECK_ALLOW_LOCAL=1 only for non-production drills"
  fi
}

load_env_file

require_secret JWT_SECRET 32
require_secret AUTH_INTERNAL_TOKEN 32
require_secret ORCHESTRATOR_INTERNAL_TOKEN 32
require_secret OJOS_WORKER_TOKEN 32
require_secret REDIS_PASSWORD 20
require_secret MINIO_ROOT_USER 8
require_secret MINIO_ROOT_PASSWORD 32
require_secret MINIO_ACCESS_KEY 8
require_secret MINIO_SECRET_KEY 32
require_secret AUTH_POSTGRES_PASSWORD 20
require_secret PROBLEM_POSTGRES_PASSWORD 20
require_secret JUDGE_POSTGRES_PASSWORD 20
require_secret USER_POSTGRES_PASSWORD 20
require_secret ORCHESTRATOR_POSTGRES_PASSWORD 20

require_database_url AUTH_DATABASE_URL
require_database_url PROBLEM_DATABASE_URL
require_database_url JUDGE_DATABASE_URL
require_database_url USER_DATABASE_URL
require_database_url ORCHESTRATOR_DATABASE_URL
require_redis_url REDIS_URL

if [[ "${OJOS_SECRET_CHECK_REQUIRE_ALERTS:-0}" == "1" ]]; then
  alert_url="$(value_for OJOS_ALERT_WEBHOOK_URL)"
  [[ "$alert_url" =~ ^https:// ]] || die "OJOS_ALERT_WEBHOOK_URL must be an https:// URL"
  reject_weak_value OJOS_ALERT_WEBHOOK_URL "$alert_url"
fi

if [[ "${OJOS_SECRET_CHECK_REQUIRE_MONITORING:-0}" == "1" ]]; then
  require_secret GRAFANA_ADMIN_PASSWORD 32
fi

echo "secret-check: production secret policy passed"
