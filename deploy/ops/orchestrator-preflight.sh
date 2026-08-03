#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "orchestrator-preflight: $*" >&2; exit 1; }
required() {
  local name="$1"
  [[ -n "${!name:-}" ]] || die "$name is required"
}
readable_nonempty() {
  local name="$1" path="${!1:-}"
  required "$name"
  [[ -f "$path" && -r "$path" && -s "$path" ]] || die "$name must name a readable, non-empty file"
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

command -v jq >/dev/null 2>&1 || die "jq is required"
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
[[ -z "$gateway" && -z "$auth" || -n "$gateway" && -n "$auth" ]] || \
  die "Gateway and Auth management providers must be configured together"
for value in "$gateway" "$auth"; do
  [[ -z "$value" || "$value" == https://* || "$value" == http://127.0.0.1:* || "$value" == http://localhost:* ]] || \
    die "provider origins must use HTTPS (HTTP is limited to loopback)"
done

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
