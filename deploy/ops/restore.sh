#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

load_env_file() {
  local env_file="${OJOS_ENV_FILE:-}"
  if [[ -n "$env_file" ]]; then
    if [[ ! -f "$env_file" ]]; then
      echo "OJOS_ENV_FILE does not exist: $env_file" >&2
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
  echo "restore: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

require_env() {
  local name="$1"
  local value="${!name:-}"
  [[ -n "$value" ]] || die "$name is required"
  printf '%s' "$value"
}

load_env_file

need_cmd sha256sum
need_cmd pg_restore
need_cmd tar

restore_dir="${OJOS_RESTORE_DIR:-${1:-}}"
[[ -n "$restore_dir" ]] || die "set OJOS_RESTORE_DIR or pass a backup directory"
[[ -d "$restore_dir" ]] || die "backup directory does not exist: $restore_dir"

environment="${OJOS_ENVIRONMENT:-production}"
expected_confirm="restore-$environment"
[[ "${OJOS_CONFIRM_RESTORE:-}" == "$expected_confirm" ]] || die "set OJOS_CONFIRM_RESTORE=$expected_confirm to confirm destructive restore"

if [[ -f "$restore_dir/SHA256SUMS" ]]; then
  (cd "$restore_dir" && sha256sum -c SHA256SUMS)
else
  die "SHA256SUMS is missing from backup directory"
fi

declare -a db_specs=(
  "orchestrator:ORCHESTRATOR_DATABASE_URL"
  "auth:AUTH_DATABASE_URL"
  "problem:PROBLEM_DATABASE_URL"
  "judge:JUDGE_DATABASE_URL"
  "user:USER_DATABASE_URL"
)

for spec in "${db_specs[@]}"; do
  name="${spec%%:*}"
  var="${spec##*:}"
  dump="$restore_dir/postgres/$name.dump"
  [[ -f "$dump" ]] || die "database dump missing: $dump"
  url="$(require_env "$var")"
  echo "restore: restoring $name database"
  pg_restore --clean --if-exists --no-owner --no-acl --dbname "$url" "$dump"
done

if [[ -f "$restore_dir/redis/dump.rdb" && "${OJOS_RESTORE_SKIP_REDIS:-0}" != "1" ]]; then
  redis_rdb_path="$(require_env OJOS_REDIS_RDB_PATH)"
  echo "restore: installing Redis RDB at $redis_rdb_path"
  install -m 0600 "$restore_dir/redis/dump.rdb" "$redis_rdb_path"
  echo "restore: restart Redis after this script completes so it loads the restored RDB"
fi

if [[ -f "$restore_dir/storage/storage-root.tar.gz" && "${OJOS_RESTORE_SKIP_STORAGE:-0}" != "1" ]]; then
  storage_root="$(require_env OJOS_STORAGE_ROOT)"
  mkdir -p "$storage_root"
  echo "restore: extracting local storage root"
  tar -C "$storage_root" -xzf "$restore_dir/storage/storage-root.tar.gz"
fi

if [[ -d "$restore_dir/storage/minio" && "${OJOS_RESTORE_SKIP_STORAGE:-0}" != "1" ]]; then
  need_cmd mc
  endpoint="${OJOS_RESTORE_MINIO_ENDPOINT:-${MINIO_ENDPOINT:-}}"
  [[ -n "$endpoint" ]] || die "MINIO_ENDPOINT is required to restore MinIO buckets"
  access_key="${MINIO_ACCESS_KEY:-${MINIO_ROOT_USER:-}}"
  secret_key="${MINIO_SECRET_KEY:-${MINIO_ROOT_PASSWORD:-}}"
  [[ -n "$access_key" ]] || die "MINIO_ACCESS_KEY or MINIO_ROOT_USER is required"
  [[ -n "$secret_key" ]] || die "MINIO_SECRET_KEY or MINIO_ROOT_PASSWORD is required"
  if [[ "$endpoint" != http://* && "$endpoint" != https://* ]]; then
    if [[ "${MINIO_USE_SSL:-false}" == "true" ]]; then
      endpoint="https://$endpoint"
    else
      endpoint="http://$endpoint"
    fi
  fi
  alias_name="${OJOS_RESTORE_MINIO_ALIAS:-ojos-restore}"
  mc alias set "$alias_name" "$endpoint" "$access_key" "$secret_key" >/dev/null
  for bucket_dir in "$restore_dir"/storage/minio/*; do
    [[ -d "$bucket_dir" ]] || continue
    bucket="$(basename "$bucket_dir")"
    echo "restore: mirroring MinIO bucket $bucket"
    mc mb --ignore-existing "$alias_name/$bucket" >/dev/null
    mc mirror --overwrite --remove "$bucket_dir" "$alias_name/$bucket"
  done
fi

echo "restore: completed from $restore_dir"
echo "restore: run $script_dir/preflight.sh before reopening production traffic"
