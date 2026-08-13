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

require_absolute_path() {
  local name="$1"
  local value="${!name:-}"
  [[ -n "$value" ]] || die "$name is required"
  reject_weak_value "$name" "$value"
  [[ "$value" == /* ]] || die "$name must be an absolute path"
}

require_private_token_file() {
  local name="$1" min_len="$2" max_len="$3" path="${!1:-}"
  [[ -n "$path" ]] || die "$name is required"
  [[ "$path" == /* ]] || die "$name must be an absolute host path"
  [[ ! -L "$path" && -f "$path" ]] || die "$name must name a regular file, not a symlink"
  [[ -r "$path" && -s "$path" ]] || die "$name must name a readable, non-empty file"

  local metadata owner mode size token
  metadata="$(stat -c '%u:%g %a' "$path" 2>/dev/null || stat -f '%u:%g %Lp' "$path" 2>/dev/null)" || \
    die "cannot inspect $name ownership and permissions"
  owner="${metadata% *}"
  mode="${metadata##* }"
  [[ "$owner" == "65532:65532" ]] || \
    die "$name must be owned by exact Auth runtime uid/gid 65532:65532"
  mode="${mode#0}"
  [[ "$mode" =~ ^[0-7]{3}$ ]] || die "cannot parse $name permissions"
  [[ "$mode" == "600" ]] || die "$name must use exact mode 0600"

  size="$(wc -c <"$path")"
  [[ "$size" =~ ^[0-9]+$ ]] || die "cannot inspect $name size"
  (( size >= min_len && size <= max_len + 1 )) || \
    die "$name must contain a $min_len-$max_len character token plus at most one trailing newline"
  token="$(<"$path")"
  (( ${#token} >= min_len && ${#token} <= max_len )) || \
    die "$name token must contain between $min_len and $max_len characters"
  (( size == ${#token} || size == ${#token} + 1 )) || \
    die "$name may contain only the token and one optional trailing newline"
  if (( size == ${#token} + 1 )); then
    [[ "$(tail -c 1 "$path" | od -An -tu1 | tr -d '[:space:]')" == "10" ]] || \
      die "$name may contain only one LF trailing newline"
  fi
  [[ "$token" =~ ^[A-Za-z0-9_-]+$ ]] || \
    die "$name token must use only URL-safe A-Z, a-z, 0-9, underscore, and hyphen characters"
}

require_token_file_distinct_from() {
  local file_name="$1" path="${!1:-}" token other_name other_value
  token="$(<"$path")"
  shift
  for other_name in "$@"; do
    other_value="$(value_for "$other_name")"
    [[ -z "$other_value" || "$token" != "$other_value" ]] || \
      die "$file_name must not reuse $other_name"
  done
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

[[ -z "${AUTH_ADMIN_BOOTSTRAP_SECRET:-}" ]] || \
  die "AUTH_ADMIN_BOOTSTRAP_SECRET is forbidden in production; use AUTH_ADMIN_BOOTSTRAP_SECRET_FILE"
[[ -z "${OJOS_SECRET_ADMINBOOTSTRAP_SECRET:-}" ]] || \
  die "OJOS_SECRET_ADMINBOOTSTRAP_SECRET is Agent-only and forbidden for production platform bootstrap; use AUTH_ADMIN_BOOTSTRAP_SECRET_FILE"
if [[ -n "${AUTH_ADMIN_BOOTSTRAP_SECRET_FILE:-}" ]]; then
  require_private_token_file AUTH_ADMIN_BOOTSTRAP_SECRET_FILE 32 512
  require_token_file_distinct_from AUTH_ADMIN_BOOTSTRAP_SECRET_FILE \
    JWT_SECRET \
    AUTH_INTERNAL_TOKEN \
    ORCHESTRATOR_INTERNAL_TOKEN \
    ORCHESTRATOR_OBSERVABILITY_TOKEN \
    PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN \
    ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN \
    ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN \
    ORCHESTRATOR_AUTH_WORKLOAD_TOKEN \
    ORCHESTRATOR_GATEWAY_ADMIN_TOKEN \
    ORCHESTRATOR_AUTH_ADMIN_TOKEN \
    OJOS_USER_SERVICE_TOKEN \
    OJOS_PROBLEM_SERVICE_TOKEN \
    OJOS_JUDGE_API_SERVICE_TOKEN
fi

require_secret JWT_SECRET 32
require_secret AUTH_INTERNAL_TOKEN 32
require_secret ORCHESTRATOR_INTERNAL_TOKEN 32
require_secret ORCHESTRATOR_OBSERVABILITY_TOKEN 32
require_secret PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN 32
require_secret ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN 32
require_secret ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN 32
require_secret ORCHESTRATOR_AUTH_WORKLOAD_TOKEN 32
require_secret ORCHESTRATOR_GATEWAY_ADMIN_TOKEN 32
require_secret ORCHESTRATOR_AUTH_ADMIN_TOKEN 32
require_enabled_flag ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM
require_secret REDIS_PASSWORD 20
require_secret MINIO_ROOT_USER 8
require_secret MINIO_ROOT_PASSWORD 32
require_secret MINIO_ACCESS_KEY 8
require_secret MINIO_SECRET_KEY 32
require_secret AUTH_POSTGRES_PASSWORD 20
require_secret ORCHESTRATOR_POSTGRES_PASSWORD 20

# 身份边界不同的 token 必须使用不同的值。长度足够并不能阻止一个泄漏的
# service token 被拿去调用内部管理接口，因此生产预检在这里直接拒绝复用。
require_all_distinct_secrets \
  JWT_SECRET \
  AUTH_INTERNAL_TOKEN \
  ORCHESTRATOR_INTERNAL_TOKEN \
  ORCHESTRATOR_OBSERVABILITY_TOKEN \
  ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN \
  ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN \
  ORCHESTRATOR_AUTH_WORKLOAD_TOKEN \
  ORCHESTRATOR_GATEWAY_ADMIN_TOKEN \
  ORCHESTRATOR_AUTH_ADMIN_TOKEN \
  OJOS_USER_SERVICE_TOKEN \
  OJOS_PROBLEM_SERVICE_TOKEN \
  OJOS_JUDGE_API_SERVICE_TOKEN

[[ "$(value_for ORCHESTRATOR_OBSERVABILITY_TOKEN)" == \
   "$(value_for PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN)" ]] || \
  die "Prometheus observability token copy must match ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE"

require_sha256_verifier() {
  local name="$1"
  local raw_name="$2"
  local verifier expected
  verifier="$(value_for "$name")"
  [[ "$verifier" =~ ^sha256:[0-9a-f]{64}$ ]] || \
    die "$name must be canonical sha256:<64 lowercase hex>"
  if command -v sha256sum >/dev/null 2>&1; then
    expected="sha256:$(printf '%s' "$(value_for "$raw_name")" | sha256sum | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    expected="sha256:$(printf '%s' "$(value_for "$raw_name")" | shasum -a 256 | awk '{print $1}')"
  else
    die "sha256sum or shasum is required to verify Contribution ACK credentials"
  fi
  [[ "$verifier" == "$expected" ]] || die "$name does not match $raw_name"
}

# The daemon receives only verifiers; the two raw values are distributed to
# their target services by Compose or signed runtime secret materialization.
require_sha256_verifier ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256 \
  ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN
require_sha256_verifier ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256 \
  ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN

require_database_url AUTH_DATABASE_URL
require_database_url ORCHESTRATOR_DATABASE_URL
require_database_url ORCHESTRATOR_MIGRATION_DATABASE_URL
require_redis_url REDIS_URL
[[ "$(value_for ORCHESTRATOR_DATABASE_URL)" =~ [\?\&]sslmode=require([\&]|$) ]] || \
  die "ORCHESTRATOR_DATABASE_URL must explicitly set sslmode=require"
migration_database_url="$(value_for ORCHESTRATOR_MIGRATION_DATABASE_URL)"
[[ "$migration_database_url" =~ [\?\&]sslmode=verify-full([\&]|$) ]] || \
  die "ORCHESTRATOR_MIGRATION_DATABASE_URL must explicitly set sslmode=verify-full"
[[ "$migration_database_url" == *"sslrootcert=/run/secrets/orchestrator-postgres-ca.crt"* ]] || \
  die "ORCHESTRATOR_MIGRATION_DATABASE_URL must use the mounted PostgreSQL CA path"
orchestrator_database_name="${ORCHESTRATOR_POSTGRES_DB:-ojos_orchestrator}"
migration_database_path="${migration_database_url%%\?*}"
[[ "${migration_database_path##*/}" == "$orchestrator_database_name" ]] || \
  die "ORCHESTRATOR_MIGRATION_DATABASE_URL must target ORCHESTRATOR_POSTGRES_DB"
for name in \
  ORCHESTRATOR_POSTGRES_CA_CERT \
  ORCHESTRATOR_HEALTHCHECK_CA_CERT \
  ORCHESTRATOR_TLS_CERT \
  ORCHESTRATOR_TLS_KEY \
  ORCHESTRATOR_NODE_CA_CERT \
  ORCHESTRATOR_NODE_CA_KEY \
  ORCHESTRATOR_GATEWAY_WORKLOAD_CA_CERT \
  ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE \
  PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE \
  OJOS_WORKLOAD_PRIVATE_KEY_FILE \
  OJOS_WORKLOAD_PUBLIC_KEY_FILE \
  ORCHESTRATOR_ARTIFACT_DIR
do
  require_absolute_path "$name"
done
healthcheck_url="$(value_for ORCHESTRATOR_HEALTHCHECK_URL)"
[[ "$healthcheck_url" == https://* ]] || \
  die "ORCHESTRATOR_HEALTHCHECK_URL must use https:// in production"
gateway_workload_origin="$(value_for ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN)"
[[ "$gateway_workload_origin" == https://* ]] || \
  die "ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN must use https:// in production"
platform_origin_policy() {
  local name="$1" compose_host="$2" value
  value="$(value_for "$name")"
  [[ -n "$value" ]] || die "$name is required"
  if [[ "$value" == https://* || "$value" == http://127.0.0.1:* || "$value" == http://localhost:* ]]; then
    return
  fi
  if [[ "$(value_for ORCHESTRATOR_ALLOW_COMPOSE_BOOTSTRAP_HTTP)" == "1" && \
        ( "$value" == "http://${compose_host}" || "$value" == "http://${compose_host}:"* ) ]]; then
    return
  fi
  die "$name must use HTTPS or the explicitly enabled exact $compose_host Compose bootstrap origin"
}
platform_origin_policy ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN auth-service
platform_origin_policy ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN gateway
platform_origin_policy ORCHESTRATOR_AUTH_ADMIN_ORIGIN auth-service
require_secret ORCHESTRATOR_GATEWAY_ADMIN_TOKEN 32
require_secret ORCHESTRATOR_AUTH_ADMIN_TOKEN 32
gateway_observability_origin="$(value_for ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN)"
[[ "$gateway_observability_origin" == https://* ]] || \
  die "ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN must use https:// in production"
[[ "$gateway_observability_origin" != *"@"* && \
   "$gateway_observability_origin" != *"?"* && \
   "$gateway_observability_origin" != *"#"* ]] || \
  die "ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN must be a credential-free HTTPS origin"
case "${gateway_observability_origin#https://}" in
  gateway|gateway/|gateway:*|auth-service|auth-service/*|auth-service:*|\
  judge-api|judge-api/*|judge-api:*|problem-service|problem-service/*|problem-service:*|\
  storage-service|storage-service/*|storage-service:*|user-service|user-service/*|user-service:*)
    die "ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN must not use legacy Compose DNS"
    ;;
esac
for removed in \
  ORCHESTRATOR_NODE_DISPATCH \
  ORCHESTRATOR_NODE_ENDPOINT \
  ORCHESTRATOR_NODE_TOKEN \
  ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER \
  ORCHESTRATOR_NODE_HOST_IP
do
  [[ -z "$(value_for "$removed")" ]] || \
    die "$removed belongs to the removed 0.2 Node push/bearer path; use the v1 mTLS pull Agent"
done

if [[ "${OJOS_SECRET_CHECK_REQUIRE_ALERTS:-0}" == "1" ]]; then
  alert_url="$(value_for OJOS_ALERT_WEBHOOK_URL)"
  [[ "$alert_url" =~ ^https:// ]] || die "OJOS_ALERT_WEBHOOK_URL must be an https:// URL"
  reject_weak_value OJOS_ALERT_WEBHOOK_URL "$alert_url"
fi

if [[ "${OJOS_SECRET_CHECK_REQUIRE_MONITORING:-0}" == "1" ]]; then
  require_secret GRAFANA_ADMIN_PASSWORD 32
fi

# Optional Redis/MinIO transport enforcement is separate from the mandatory
# Orchestrator HTTPS and Node mTLS files checked above. Local integration may
# keep loopback-only Redis/MinIO plaintext; production can require both here.
if [[ "${OJOS_SECRET_CHECK_REQUIRE_TLS:-0}" == "1" ]]; then
  redis_url="$(value_for REDIS_URL)"
  [[ "$redis_url" =~ ^rediss:// ]] || die "OJOS_SECRET_CHECK_REQUIRE_TLS=1 requires REDIS_URL to use the rediss:// (TLS) scheme"
  minio_ssl="$(printf '%s' "$(value_for MINIO_USE_SSL)" | tr '[:upper:]' '[:lower:]')"
  [[ "$minio_ssl" == "true" ]] || die "OJOS_SECRET_CHECK_REQUIRE_TLS=1 requires MINIO_USE_SSL=true"
fi

echo "secret-check: production secret policy passed"
