#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "orchestrator-preflight: $*" >&2; exit 1; }
load_env_file() {
  local env_file="${OJOS_ENV_FILE:-}"
  [[ -n "$env_file" ]] || return
  [[ -f "$env_file" ]] || die "OJOS_ENV_FILE does not exist: $env_file"
  local line key value
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
}
required() {
  local name="$1"
  [[ -n "${!name:-}" ]] || die "$name is required"
}
readable_nonempty() {
  local name="$1" path="${!1:-}"
  required "$name"
  [[ -f "$path" && -r "$path" && -s "$path" ]] || die "$name must name a readable, non-empty file"
}
private_token_file() {
  local name="$1" min_len="$2" max_len="$3" path="${!1:-}"
  required "$name"
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
token_file_distinct_from() {
  local file_name="$1" path="${!1:-}" token other_name other_value
  token="$(<"$path")"
  shift
  for other_name in "$@"; do
    other_value="${!other_name:-}"
    [[ -z "$other_value" || "$token" != "$other_value" ]] || \
      die "$file_name must not reuse $other_name"
  done
}
writable_directory() {
  local name="$1" path="${!1:-}"
  required "$name"
  [[ -d "$path" && -w "$path" ]] || die "$name must name an existing writable directory"
}
https_url() {
  local name="$1" value="${!1:-}"
  required "$name"
  [[ "$value" == https://* ]] || die "$name must use https://"
}
platform_origin() {
  local name="$1" compose_host="$2" value="${!1:-}"
  required "$name"
  if [[ "$value" == https://* || "$value" == http://127.0.0.1:* || "$value" == http://localhost:* ]]; then
    return
  fi
  if [[ "${ORCHESTRATOR_ALLOW_COMPOSE_BOOTSTRAP_HTTP:-0}" == "1" && \
        ( "$value" == "http://${compose_host}" || "$value" == "http://${compose_host}:"* ) ]]; then
    return
  fi
  die "$name must use HTTPS; HTTP is limited to loopback or the exact $compose_host name on the explicitly enabled Compose bootstrap network"
}

load_env_file
command -v jq >/dev/null 2>&1 || die "jq is required"
[[ -z "${AUTH_ADMIN_BOOTSTRAP_SECRET:-}" ]] || \
  die "AUTH_ADMIN_BOOTSTRAP_SECRET is forbidden in production; use AUTH_ADMIN_BOOTSTRAP_SECRET_FILE"
[[ -z "${OJOS_SECRET_ADMINBOOTSTRAP_SECRET:-}" ]] || \
  die "OJOS_SECRET_ADMINBOOTSTRAP_SECRET is Agent-only and forbidden for production platform bootstrap; use AUTH_ADMIN_BOOTSTRAP_SECRET_FILE"
if [[ -n "${AUTH_ADMIN_BOOTSTRAP_SECRET_FILE:-}" ]]; then
  private_token_file AUTH_ADMIN_BOOTSTRAP_SECRET_FILE 32 512
  token_file_distinct_from AUTH_ADMIN_BOOTSTRAP_SECRET_FILE \
    JWT_SECRET \
    AUTH_INTERNAL_TOKEN \
    ORCHESTRATOR_INTERNAL_TOKEN \
    ORCHESTRATOR_OBSERVABILITY_TOKEN \
    ORCHESTRATOR_AUTH_WORKLOAD_TOKEN \
    ORCHESTRATOR_GATEWAY_ADMIN_TOKEN \
    ORCHESTRATOR_AUTH_ADMIN_TOKEN \
    ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN \
    ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN \
    OJOS_USER_SERVICE_TOKEN \
    OJOS_PROBLEM_SERVICE_TOKEN \
    OJOS_JUDGE_API_SERVICE_TOKEN
fi
required ORCHESTRATOR_DATABASE_URL
[[ "$ORCHESTRATOR_DATABASE_URL" == postgres://* || "$ORCHESTRATOR_DATABASE_URL" == postgresql://* ]] || \
  die "ORCHESTRATOR_DATABASE_URL must be a PostgreSQL URL"
[[ "$ORCHESTRATOR_DATABASE_URL" =~ [\?\&]sslmode=require([\&]|$) ]] || \
  die "ORCHESTRATOR_DATABASE_URL must explicitly set sslmode=require"
required ORCHESTRATOR_MIGRATION_DATABASE_URL
[[ "$ORCHESTRATOR_MIGRATION_DATABASE_URL" =~ [\?\&]sslmode=verify-full([\&]|$) ]] || \
  die "ORCHESTRATOR_MIGRATION_DATABASE_URL must explicitly set sslmode=verify-full"
[[ "$ORCHESTRATOR_MIGRATION_DATABASE_URL" == *"sslrootcert=/run/secrets/orchestrator-postgres-ca.crt"* ]] || \
  die "ORCHESTRATOR_MIGRATION_DATABASE_URL must use /run/secrets/orchestrator-postgres-ca.crt"
orchestrator_database_name="${ORCHESTRATOR_POSTGRES_DB:-ojos_orchestrator}"
migration_database_path="${ORCHESTRATOR_MIGRATION_DATABASE_URL%%\?*}"
[[ "${migration_database_path##*/}" == "$orchestrator_database_name" ]] || \
  die "ORCHESTRATOR_MIGRATION_DATABASE_URL must target ORCHESTRATOR_POSTGRES_DB"
readable_nonempty ORCHESTRATOR_POSTGRES_CA_CERT
writable_directory ORCHESTRATOR_ARTIFACT_DIR

https_url ORCHESTRATOR_HEALTHCHECK_URL
readable_nonempty ORCHESTRATOR_HEALTHCHECK_CA_CERT
https_url ORCHESTRATOR_OIDC_ISSUER
required ORCHESTRATOR_OIDC_AUDIENCE
required ORCHESTRATOR_OIDC_CLIENT_ID
https_url ORCHESTRATOR_PUBLIC_BASE_URL

for name in ORCHESTRATOR_TLS_CERT ORCHESTRATOR_TLS_KEY ORCHESTRATOR_NODE_CA_CERT ORCHESTRATOR_NODE_CA_KEY; do
  readable_nonempty "$name"
done
required ORCHESTRATOR_AUTH_WORKLOAD_TOKEN
platform_origin ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN auth-service
https_url ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN
https_url ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN
case "${ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN#https://}" in
  gateway|gateway/|gateway:*|auth-service|auth-service/*|auth-service:*|\
  judge-api|judge-api/*|judge-api:*|problem-service|problem-service/*|problem-service:*|\
  storage-service|storage-service/*|storage-service:*|user-service|user-service/*|user-service:*)
    die "ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN must not use legacy Compose DNS"
    ;;
esac
readable_nonempty ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE
for name in ORCHESTRATOR_GATEWAY_WORKLOAD_CA_CERT OJOS_WORKLOAD_PRIVATE_KEY_FILE OJOS_WORKLOAD_PUBLIC_KEY_FILE; do
  readable_nonempty "$name"
done

required ORCHESTRATOR_CATALOG_TRUST_KEYS
required ORCHESTRATOR_CATALOG_SOURCES
jq -e 'type == "object" and length > 0 and all(.[]; type == "string" and length > 0)' \
  <<<"$ORCHESTRATOR_CATALOG_TRUST_KEYS" >/dev/null || \
  die "ORCHESTRATOR_CATALOG_TRUST_KEYS must be a non-empty key-id/public-key object"
jq -e 'type == "array" and length > 0 and all(.[]; (.id|type=="string") and (.url|type=="string") and (.required_key_id|type=="string"))' \
  <<<"$ORCHESTRATOR_CATALOG_SOURCES" >/dev/null || \
  die "ORCHESTRATOR_CATALOG_SOURCES must be a non-empty CatalogSource array"

gateway="${ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN:-}"
auth="${ORCHESTRATOR_AUTH_ADMIN_ORIGIN:-}"
platform_origin ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN gateway
platform_origin ORCHESTRATOR_AUTH_ADMIN_ORIGIN auth-service
required ORCHESTRATOR_GATEWAY_ADMIN_TOKEN
required ORCHESTRATOR_AUTH_ADMIN_TOKEN

web_root="${ORCHESTRATOR_WEB_ROOT:-manager/web/dist}"
[[ -s "$web_root/index.html" ]] || die "built Web UI is missing at $web_root/index.html"

if [[ -n "${ORCHESTRATOR_MAX_WORKERS:-}" ]]; then
  [[ "$ORCHESTRATOR_MAX_WORKERS" =~ ^[0-9]+$ && "$ORCHESTRATOR_MAX_WORKERS" -ge 128 ]] || \
    die "ORCHESTRATOR_MAX_WORKERS must be at least 128 in production"
fi
if [[ -n "${ORCHESTRATOR_LOG_RETENTION_DAYS:-}" ]]; then
  [[ "$ORCHESTRATOR_LOG_RETENTION_DAYS" =~ ^[0-9]+$ && \
     "$ORCHESTRATOR_LOG_RETENTION_DAYS" -ge 1 && \
     "$ORCHESTRATOR_LOG_RETENTION_DAYS" -le 3650 ]] || \
    die "ORCHESTRATOR_LOG_RETENTION_DAYS must be between 1 and 3650"
fi
if [[ -n "${ORCHESTRATOR_ARTIFACT_RETENTION_DAYS:-}" ]]; then
  [[ "$ORCHESTRATOR_ARTIFACT_RETENTION_DAYS" =~ ^[0-9]+$ && \
     "$ORCHESTRATOR_ARTIFACT_RETENTION_DAYS" -ge 1 && \
     "$ORCHESTRATOR_ARTIFACT_RETENTION_DAYS" -le 3650 ]] || \
    die "ORCHESTRATOR_ARTIFACT_RETENTION_DAYS must be between 1 and 3650"
fi
if [[ -n "${ORCHESTRATOR_ARTIFACT_QUOTA_BYTES:-}" ]]; then
  [[ "$ORCHESTRATOR_ARTIFACT_QUOTA_BYTES" =~ ^[0-9]+$ && \
     "$ORCHESTRATOR_ARTIFACT_QUOTA_BYTES" -ge 1048576 ]] || \
    die "ORCHESTRATOR_ARTIFACT_QUOTA_BYTES must be at least 1048576"
fi

if [[ -n "${ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT:-}" ]]; then
  [[ "$ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT" == https://* || \
     "$ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT" == http://127.0.0.1:* || \
     "$ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT" == http://localhost:* ]] || \
    die "OTLP endpoint must use HTTPS (HTTP is limited to loopback)"
fi

echo "orchestrator-preflight: production configuration is complete; daemon startup will verify PostgreSQL, migrations, OIDC discovery/JWKS, certificates, Catalog signatures and the single-active lock"
