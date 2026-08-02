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

require_enabled_flag() {
  local name="$1"
  local value
  value="$(printf '%s' "$(value_for "$name")" | tr '[:upper:]' '[:lower:]')"
  case "$value" in
    1|true|yes|on) ;;
    *) die "$name must be enabled in production" ;;
  esac
}

flag_is_enabled() {
  local value
  value="$(printf '%s' "$(value_for "$1")" | tr '[:upper:]' '[:lower:]')"
  [[ "$value" =~ ^(1|true|yes|on)$ ]]
}

require_distinct_secret() {
  local name="$1"
  shift
  local value
  value="$(value_for "$name")"
  local other_name other_value
  for other_name in "$@"; do
    other_value="$(value_for "$other_name")"
    if [[ -n "$other_value" && "$value" == "$other_value" ]]; then
      die "$name must not reuse $other_name"
    fi
  done
}

require_all_distinct_secrets() {
  local names=("$@")
  local index other_index value other_value
  for ((index = 0; index < ${#names[@]}; index += 1)); do
    value="$(value_for "${names[$index]}")"
    for ((other_index = index + 1; other_index < ${#names[@]}; other_index += 1)); do
      other_value="$(value_for "${names[$other_index]}")"
      if [[ -n "$value" && "$value" == "$other_value" ]]; then
        die "${names[$index]} must not reuse ${names[$other_index]}"
      fi
    done
  done
}

load_env_file

require_secret JWT_SECRET 32
require_secret AUTH_INTERNAL_TOKEN 32
require_secret ORCHESTRATOR_INTERNAL_TOKEN 32
require_enabled_flag ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM
require_secret OJOS_WORKER_TOKEN 32
require_secret OJOS_USER_SERVICE_TOKEN 32
require_secret OJOS_PROBLEM_SERVICE_TOKEN 32
require_secret OJOS_JUDGE_API_SERVICE_TOKEN 32
require_secret OJOS_JUDGE_WORKER_SERVICE_TOKEN 32
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

# 身份边界不同的 token 必须使用不同的值。长度足够并不能阻止一个泄漏的
# service token 被拿去调用内部管理接口，因此生产预检在这里直接拒绝复用。
require_all_distinct_secrets \
  JWT_SECRET \
  AUTH_INTERNAL_TOKEN \
  ORCHESTRATOR_INTERNAL_TOKEN \
  OJOS_WORKER_TOKEN \
  OJOS_USER_SERVICE_TOKEN \
  OJOS_PROBLEM_SERVICE_TOKEN \
  OJOS_JUDGE_API_SERVICE_TOKEN \
  OJOS_JUDGE_WORKER_SERVICE_TOKEN

require_database_url AUTH_DATABASE_URL
require_database_url PROBLEM_DATABASE_URL
require_database_url JUDGE_DATABASE_URL
require_database_url USER_DATABASE_URL
require_database_url ORCHESTRATOR_DATABASE_URL
require_redis_url REDIS_URL

if flag_is_enabled ORCHESTRATOR_NODE_DISPATCH \
  || flag_is_enabled ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER; then
  require_secret ORCHESTRATOR_NODE_TOKEN 32
  require_distinct_secret ORCHESTRATOR_NODE_TOKEN \
    JWT_SECRET \
    ORCHESTRATOR_INTERNAL_TOKEN \
    AUTH_INTERNAL_TOKEN \
    OJOS_WORKER_TOKEN \
    OJOS_USER_SERVICE_TOKEN \
    OJOS_PROBLEM_SERVICE_TOKEN \
    OJOS_JUDGE_API_SERVICE_TOKEN \
    OJOS_JUDGE_WORKER_SERVICE_TOKEN
fi

if flag_is_enabled ORCHESTRATOR_NODE_DISPATCH; then
  node_endpoint="$(value_for ORCHESTRATOR_NODE_ENDPOINT)"
  [[ "$node_endpoint" =~ ^https?://[^[:space:]]+$ ]] \
    || die "ORCHESTRATOR_NODE_ENDPOINT must be an http(s) URL when node dispatch is enabled"
  reject_weak_value ORCHESTRATOR_NODE_ENDPOINT "$node_endpoint"
fi

if flag_is_enabled ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER; then
  node_host_ip="$(value_for ORCHESTRATOR_NODE_HOST_IP)"
  [[ -n "$node_host_ip" ]] || die "ORCHESTRATOR_NODE_HOST_IP is required when node driver execution is enabled"
  reject_weak_value ORCHESTRATOR_NODE_HOST_IP "$node_host_ip"
fi

if [[ "${OJOS_SECRET_CHECK_REQUIRE_ALERTS:-0}" == "1" ]]; then
  alert_url="$(value_for OJOS_ALERT_WEBHOOK_URL)"
  [[ "$alert_url" =~ ^https:// ]] || die "OJOS_ALERT_WEBHOOK_URL must be an https:// URL"
  reject_weak_value OJOS_ALERT_WEBHOOK_URL "$alert_url"
fi

if [[ "${OJOS_SECRET_CHECK_REQUIRE_MONITORING:-0}" == "1" ]]; then
  require_secret GRAFANA_ADMIN_PASSWORD 32
fi

# Opt-in transport-security enforcement. Off by default so the current beta profile
# (loopback-bound datastores, plaintext redis://, MINIO_USE_SSL=false) and ci-policy.sh
# keep passing. Turning it on requires TLS-configured Redis and MinIO endpoints, which
# depends on a PKI/cert decision that is still deferred.
if [[ "${OJOS_SECRET_CHECK_REQUIRE_TLS:-0}" == "1" ]]; then
  redis_url="$(value_for REDIS_URL)"
  [[ "$redis_url" =~ ^rediss:// ]] || die "OJOS_SECRET_CHECK_REQUIRE_TLS=1 requires REDIS_URL to use the rediss:// (TLS) scheme"
  minio_ssl="$(printf '%s' "$(value_for MINIO_USE_SSL)" | tr '[:upper:]' '[:lower:]')"
  [[ "$minio_ssl" == "true" ]] || die "OJOS_SECRET_CHECK_REQUIRE_TLS=1 requires MINIO_USE_SSL=true"
fi

echo "secret-check: production secret policy passed"
