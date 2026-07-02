#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

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
  echo "backup: $*" >&2
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

need_cmd date
need_cmd find
need_cmd sha256sum
need_cmd pg_dump
need_cmd tar

backup_root="$(require_env OJOS_BACKUP_DIR)"
stamp="${OJOS_BACKUP_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
backup_dir="$backup_root/$stamp"
created_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

umask 077
mkdir -p "$backup_dir/postgres" "$backup_dir/redis" "$backup_dir/storage"

declare -a db_specs=(
  "orchestrator:ORCHESTRATOR_DATABASE_URL"
  "auth:AUTH_DATABASE_URL"
  "problem:PROBLEM_DATABASE_URL"
  "judge:JUDGE_DATABASE_URL"
  "user:USER_DATABASE_URL"
)

echo "backup: writing $backup_dir"
for spec in "${db_specs[@]}"; do
  name="${spec%%:*}"
  var="${spec##*:}"
  url="$(require_env "$var")"
  out="$backup_dir/postgres/$name.dump"
  echo "backup: dumping $name database"
  pg_dump --format=custom --no-owner --no-acl --file "$out" "$url"
done

if [[ "${OJOS_BACKUP_SKIP_REDIS:-0}" != "1" ]]; then
  need_cmd redis-cli
  redis_url="$(require_env REDIS_URL)"
  echo "backup: exporting Redis RDB"
  redis-cli -u "$redis_url" --rdb "$backup_dir/redis/dump.rdb" >/dev/null
fi

storage_backed_up=0
if [[ "${OJOS_BACKUP_SKIP_STORAGE:-0}" != "1" ]]; then
  if [[ -n "${OJOS_STORAGE_ROOT:-}" ]]; then
    [[ -d "$OJOS_STORAGE_ROOT" ]] || die "OJOS_STORAGE_ROOT is not a directory: $OJOS_STORAGE_ROOT"
    echo "backup: archiving local storage root"
    tar -C "$OJOS_STORAGE_ROOT" -czf "$backup_dir/storage/storage-root.tar.gz" .
    storage_backed_up=1
  fi

  if [[ -n "${MINIO_ENDPOINT:-}" || -n "${OJOS_BACKUP_MINIO_ENDPOINT:-}" ]]; then
    need_cmd mc
    endpoint="${OJOS_BACKUP_MINIO_ENDPOINT:-${MINIO_ENDPOINT:-}}"
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
    alias_name="${OJOS_BACKUP_MINIO_ALIAS:-ojos-backup}"
    buckets="${OJOS_STORAGE_BUCKETS:-problems,submissions,judge-artifacts,avatars}"
    mkdir -p "$backup_dir/storage/minio"
    mc alias set "$alias_name" "$endpoint" "$access_key" "$secret_key" >/dev/null
    IFS=',' read -r -a bucket_list <<<"$buckets"
    for bucket in "${bucket_list[@]}"; do
      bucket="$(printf '%s' "$bucket" | xargs)"
      [[ -n "$bucket" ]] || continue
      echo "backup: mirroring MinIO bucket $bucket"
      mkdir -p "$backup_dir/storage/minio/$bucket"
      mc mirror --overwrite --remove "$alias_name/$bucket" "$backup_dir/storage/minio/$bucket"
    done
    storage_backed_up=1
  fi

  [[ "$storage_backed_up" == "1" ]] || die "storage backup is required; set OJOS_STORAGE_ROOT or MINIO_ENDPOINT/MINIO_ACCESS_KEY/MINIO_SECRET_KEY, or set OJOS_BACKUP_SKIP_STORAGE=1 explicitly"
fi

(
  cd "$backup_dir"
  find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS
)

cat >"$backup_dir/manifest.json" <<EOF
{
  "created_at": "$created_at",
  "environment": "${OJOS_ENVIRONMENT:-production}",
  "repo_root": "$repo_root",
  "databases": ["orchestrator", "auth", "problem", "judge", "user"],
  "redis": $([[ "${OJOS_BACKUP_SKIP_REDIS:-0}" == "1" ]] && printf 'false' || printf 'true'),
  "storage": $([[ "${OJOS_BACKUP_SKIP_STORAGE:-0}" == "1" ]] && printf 'false' || printf 'true')
}
EOF

if [[ -n "${OJOS_BACKUP_TEXTFILE_DIR:-}" ]]; then
  mkdir -p "$OJOS_BACKUP_TEXTFILE_DIR"
  metric_tmp="$OJOS_BACKUP_TEXTFILE_DIR/ojos_backup.prom.$$"
  {
    printf '# HELP ojos_backup_last_success_timestamp_seconds Last successful OJOS backup Unix timestamp.\n'
    printf '# TYPE ojos_backup_last_success_timestamp_seconds gauge\n'
    printf 'ojos_backup_last_success_timestamp_seconds{environment="%s"} %s\n' "${OJOS_ENVIRONMENT:-production}" "$(date -u +%s)"
  } >"$metric_tmp"
  mv "$metric_tmp" "$OJOS_BACKUP_TEXTFILE_DIR/ojos_backup.prom"
fi

echo "backup: completed $backup_dir"
