#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "backup: $*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }
require_env() {
  local name="$1" value="${!1:-}"
  [[ -n "$value" ]] || die "$name is required"
  printf '%s' "$value"
}
load_env_file() {
  local env_file="${OJOS_ENV_FILE:-}" line key value
  [[ -z "$env_file" ]] && return
  [[ -f "$env_file" ]] || die "OJOS_ENV_FILE does not exist: $env_file"
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
    # The explicitly supplied process environment wins over a defaults file.
    # This is essential for isolated restore drills and emergency overrides.
    [[ -v "$key" ]] || export "$key=$value"
  done <"$env_file"
}
safe_directory() {
  local path="$1" label="$2"
  [[ "$path" != *$'\n'* && "$path" != *$'\r'* ]] || die "$label must be one line"
  [[ "$path" == /* ]] || die "$label must be an absolute path"
  [[ "$path" != "/" && "$path" != "${HOME:-}" ]] || die "$label must be a dedicated directory"
}
paths_overlap() {
  local left="${1%/}/" right="${2%/}/"
  [[ "$left" == "$right"* || "$right" == "$left"* ]]
}
validate_shell_command() {
  local value="$1" label="$2"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* && ${#value} -le 4096 ]] || \
    die "$label must be a single line no longer than 4096 bytes"
}
normalize_bucket_list() {
  local raw="$1" bucket
  IFS=',' read -r -a raw_buckets <<<"$raw"
  for bucket in "${raw_buckets[@]}"; do
    bucket="$(printf '%s' "$bucket" | xargs)"
    [[ "$bucket" =~ ^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$ ]] || die "invalid MinIO bucket name: $bucket"
    printf '%s\n' "$bucket"
  done | sort -u
}

load_env_file
for command_name in cmp cp date find jq pg_dump pg_restore python3 sha256sum sort tar; do need_cmd "$command_name"; done

environment="${OJOS_ENVIRONMENT:-production}"
[[ "$environment" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$ ]] || die "OJOS_ENVIRONMENT is invalid"
source_id="$(require_env OJOS_BACKUP_SOURCE_ID)"
[[ "$source_id" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$ ]] || die "OJOS_BACKUP_SOURCE_ID is invalid"
expected_confirmation="backup-$environment-fenced-v1"
[[ "${OJOS_CONFIRM_QUIESCED_BACKUP:-}" == "$expected_confirmation" ]] || \
  die "drain all writers, acquire the external write fence, then set OJOS_CONFIRM_QUIESCED_BACKUP=$expected_confirmation"
[[ -n "${OJOS_BACKUP_FENCE_TOKEN:-}" ]] || die "OJOS_BACKUP_FENCE_TOKEN is required"
[[ "${OJOS_BACKUP_FENCE_TOKEN}" != *$'\n'* && "${OJOS_BACKUP_FENCE_TOKEN}" != *$'\r'* ]] || die "OJOS_BACKUP_FENCE_TOKEN must be one line"
verify_write_fence() {
  if [[ -n "${OJOS_BACKUP_FENCE_CHECK_COMMAND:-}" ]]; then
    validate_shell_command "$OJOS_BACKUP_FENCE_CHECK_COMMAND" "OJOS_BACKUP_FENCE_CHECK_COMMAND"
    OJOS_FENCE_TOKEN="$OJOS_BACKUP_FENCE_TOKEN" bash -Eeuo pipefail -c "$OJOS_BACKUP_FENCE_CHECK_COMMAND" || \
      die "external write fence check failed"
  elif [[ "${OJOS_BACKUP_ALLOW_DECLARED_FENCE:-0}" != "1" ]]; then
    die "set OJOS_BACKUP_FENCE_CHECK_COMMAND to verify the external fence; use OJOS_BACKUP_ALLOW_DECLARED_FENCE=1 only for an isolated drill"
  fi
}
verify_write_fence

backup_root="$(require_env OJOS_BACKUP_DIR)"
safe_directory "$backup_root" "OJOS_BACKUP_DIR"
[[ -d "$backup_root" && ! -L "$backup_root" ]] || \
  die "OJOS_BACKUP_DIR must be an existing real directory"
backup_root="$(cd "$backup_root" && pwd -P)"
safe_directory "$backup_root" "resolved OJOS_BACKUP_DIR"

local_storage_included=false
if [[ -n "${OJOS_STORAGE_ROOT:-}" && "${OJOS_BACKUP_SKIP_STORAGE:-0}" != "1" ]]; then
  safe_directory "$OJOS_STORAGE_ROOT" "OJOS_STORAGE_ROOT"
  [[ -d "$OJOS_STORAGE_ROOT" && ! -L "$OJOS_STORAGE_ROOT" ]] || \
    die "OJOS_STORAGE_ROOT must be a real directory, not a link: $OJOS_STORAGE_ROOT"
  OJOS_STORAGE_ROOT="$(cd "$OJOS_STORAGE_ROOT" && pwd -P)"
  safe_directory "$OJOS_STORAGE_ROOT" "resolved OJOS_STORAGE_ROOT"
  paths_overlap "$backup_root" "$OJOS_STORAGE_ROOT" && \
    die "OJOS_BACKUP_DIR and OJOS_STORAGE_ROOT must not overlap"
  local_storage_included=true
fi
stamp="${OJOS_BACKUP_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
[[ "$stamp" =~ ^[0-9]{8}T[0-9]{6}Z(-[A-Za-z0-9._-]+)?$ ]] || die "OJOS_BACKUP_STAMP is invalid"
final="$backup_root/$stamp"
temporary="$backup_root/.${stamp}.tmp.$$"
[[ ! -e "$final" && ! -e "$temporary" ]] || die "backup target already exists"
umask 077
mkdir -m 0700 "$temporary"
cleanup() { local rc=$?; [[ ! -e "$temporary" ]] || rm -rf -- "$temporary"; exit "$rc"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir -m 0700 "$temporary/postgres" "$temporary/redis" "$temporary/storage"
mkdir -m 0700 "$temporary/retained"

# The Problem service's runtime.volumes RETAIN attachment is independent of
# object storage. It is an Agent-owned Docker volume whose name and exact label
# set are derived from the stable service-instance identity. Never infer this
# component from a host path alone: a stale or foreign directory must not be
# published as a recoverable full-stack backup.
retained_owner_instance_id="$(require_env OJOS_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID)"
retained_volume_name="$(require_env OJOS_PROBLEM_RETAINED_VOLUME_NAME)"
retained_inspect="$temporary/.problem-retained-volume.inspect.json"
retained_identity="$temporary/retained/problem-packages.identity.json"
capture_retained_volume() {
  local identity_output="$1" mountpoint running
  rm -f -- "$retained_inspect"
  if [[ -n "${OJOS_BACKUP_RETAINED_VOLUME_INSPECT_FILE:-}" ]]; then
    [[ "${OJOS_BACKUP_ALLOW_INSPECT_FIXTURE:-0}" == "1" && \
       "${OJOS_BACKUP_ALLOW_DECLARED_FENCE:-0}" == "1" ]] || \
      die "retained-volume inspect fixtures are allowed only in an isolated declared-fence drill"
    [[ -f "$OJOS_BACKUP_RETAINED_VOLUME_INSPECT_FILE" && \
       ! -L "$OJOS_BACKUP_RETAINED_VOLUME_INSPECT_FILE" ]] || \
      die "OJOS_BACKUP_RETAINED_VOLUME_INSPECT_FILE must be a regular file, not a link"
    cp -- "$OJOS_BACKUP_RETAINED_VOLUME_INSPECT_FILE" "$retained_inspect"
    [[ "${OJOS_BACKUP_CONFIRM_RETAINED_VOLUME_QUIESCED:-}" == \
       "retained-volume-$environment-quiesced-v1" ]] || \
      die "confirm the isolated retained-volume fixture is quiesced"
  else
    need_cmd docker
    docker volume inspect "$retained_volume_name" >"$retained_inspect" || \
      die "cannot inspect required Problem retained volume $retained_volume_name"
    running="$(docker ps --quiet --filter "volume=$retained_volume_name" --filter status=running)"
    [[ -z "$running" ]] || \
      die "Problem retained volume is still mounted by a running container; quiesce its writer before backup"
  fi
  mountpoint="$(python3 "$script_dir/retained-volume.py" \
    --inspect "$retained_inspect" \
    --owner-instance-id "$retained_owner_instance_id" \
    --root "${OJOS_PROBLEM_RETAINED_VOLUME_ROOT:-$(jq -r '.[0].Mountpoint // empty' "$retained_inspect")}" \
    --output "$identity_output" --print-mountpoint)"
  [[ "$(jq -r '.[0].Name // empty' "$retained_inspect")" == "$retained_volume_name" ]] || \
    die "OJOS_PROBLEM_RETAINED_VOLUME_NAME does not match Docker inspect"
  printf '%s' "$mountpoint"
}

retained_root="$(capture_retained_volume "$retained_identity")"
safe_directory "$retained_root" "Problem retained volume Mountpoint"
paths_overlap "$backup_root" "$retained_root" && \
  die "OJOS_BACKUP_DIR and Problem retained volume must not overlap"
python3 "$script_dir/backup-manifest.py" inventory \
  --root "$retained_root" \
  --output "$temporary/retained/problem-packages.inventory.json"
echo "backup: archiving Agent-owned Problem retained volume $retained_volume_name"
tar -C "$retained_root" -czf "$temporary/retained/problem-packages.tar.gz" .
python3 "$script_dir/backup-manifest.py" verify-tar \
  --archive "$temporary/retained/problem-packages.tar.gz" \
  --expected-summary-json "$(jq -c '.tree' "$temporary/retained/problem-packages.inventory.json")" >/dev/null

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
  url="$(require_env "$var")"
  echo "backup: dumping $name database"
  pg_dump --format=custom --compress=9 --no-owner --no-acl \
    --file "$temporary/postgres/$name.dump" "$url"
  pg_restore --list "$temporary/postgres/$name.dump" >"$temporary/postgres/$name.dump.list"
done

redis_included=true
if [[ "${OJOS_BACKUP_SKIP_REDIS:-0}" == "1" ]]; then
  redis_included=false
else
  need_cmd redis-cli
  redis_url="$(require_env REDIS_URL)"
  echo "backup: exporting Redis RDB"
  redis-cli -u "$redis_url" --rdb "$temporary/redis/dump.rdb" >/dev/null
  if command -v redis-check-rdb >/dev/null 2>&1; then
    redis-check-rdb "$temporary/redis/dump.rdb" >"$temporary/redis/dump.rdb.check"
  elif [[ "${OJOS_BACKUP_ALLOW_UNVERIFIED_REDIS:-0}" != "1" ]]; then
    die "redis-check-rdb is required (or set OJOS_BACKUP_ALLOW_UNVERIFIED_REDIS=1 only for an isolated drill)"
  fi
fi

if [[ "$local_storage_included" == true ]]; then
  echo "backup: archiving local storage root"
  tar -C "$OJOS_STORAGE_ROOT" -czf "$temporary/storage/storage-root.tar.gz" .
fi

minio_included=false
buckets_json='[]'
if [[ "${OJOS_BACKUP_SKIP_STORAGE:-0}" != "1" && ( -n "${MINIO_ENDPOINT:-}" || -n "${OJOS_BACKUP_MINIO_ENDPOINT:-}" ) ]]; then
  need_cmd mc
  endpoint="${OJOS_BACKUP_MINIO_ENDPOINT:-${MINIO_ENDPOINT:-}}"
  access_key="${MINIO_ACCESS_KEY:-${MINIO_ROOT_USER:-}}"
  secret_key="${MINIO_SECRET_KEY:-${MINIO_ROOT_PASSWORD:-}}"
  [[ -n "$access_key" && -n "$secret_key" ]] || die "MinIO access and secret keys are required"
  if [[ "$endpoint" != http://* && "$endpoint" != https://* ]]; then
    [[ "${MINIO_USE_SSL:-false}" == "true" ]] && endpoint="https://$endpoint" || endpoint="http://$endpoint"
  fi
  alias_name="${OJOS_BACKUP_MINIO_ALIAS:-ojos-backup}"
  mapfile -t bucket_list < <(normalize_bucket_list "${OJOS_STORAGE_BUCKETS:-problems,submissions,judge-artifacts,avatars}")
  buckets_json="$(printf '%s\n' "${bucket_list[@]}" | jq -Rsc 'split("\n") | map(select(length > 0))')"
  mkdir -m 0700 "$temporary/storage/minio"
  mc alias set "$alias_name" "$endpoint" "$access_key" "$secret_key" >/dev/null
  for bucket in "${bucket_list[@]}"; do
    echo "backup: mirroring MinIO bucket $bucket"
    mkdir -m 0700 "$temporary/storage/minio/$bucket"
    mc mirror --overwrite "$alias_name/$bucket" "$temporary/storage/minio/$bucket"
  done
  minio_included=true
fi
if [[ "${OJOS_BACKUP_SKIP_STORAGE:-0}" != "1" && "$local_storage_included" != true && "$minio_included" != true ]]; then
  die "storage backup is required; configure local storage or MinIO, or explicitly set OJOS_BACKUP_SKIP_STORAGE=1"
fi

# Re-prove the fence after the last component snapshot. A lease that expired
# during a long dump must never produce a published cross-component backup.
verify_write_fence

# Re-inspect both the stable Docker identity and the live tree immediately
# before publishing. This catches replacement, remount and late-write races.
retained_identity_after="$temporary/retained/.problem-packages.identity.after.json"
retained_root_after="$(capture_retained_volume "$retained_identity_after")"
[[ "$retained_root_after" == "$retained_root" ]] || die "Problem retained volume Mountpoint changed during backup"
cmp -s "$retained_identity" "$retained_identity_after" || die "Problem retained volume identity changed during backup"
rm -f -- "$retained_identity_after" "$retained_inspect"
retained_inventory_after="$temporary/retained/.problem-packages.inventory.after.json"
python3 "$script_dir/backup-manifest.py" inventory \
  --root "$retained_root" --output "$retained_inventory_after"
cmp -s "$temporary/retained/problem-packages.inventory.json" "$retained_inventory_after" || \
  die "Problem retained volume live tree changed during backup"
rm -f -- "$retained_inventory_after"

create_args=(
  create
  --root "$temporary" --environment "$environment" --source-id "$source_id"
  --created-at "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  --fence-id-sha256 "$(printf '%s' "$OJOS_BACKUP_FENCE_TOKEN" | sha256sum | awk '{print $1}')"
  --redis "$redis_included" --local-storage "$local_storage_included"
  --minio "$minio_included" --buckets-json "$buckets_json"
  --problem-retained-volume-source "$retained_root"
)
[[ "$local_storage_included" != true ]] || create_args+=(--local-storage-source "$OJOS_STORAGE_ROOT")
python3 "$script_dir/backup-manifest.py" "${create_args[@]}"
(
  cd "$temporary"
  find . -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  sha256sum -c SHA256SUMS >/dev/null
)
# Keep the externally held fence and source volume facts valid through the
# last possible moment before atomic publication.
verify_write_fence
retained_identity_final="$temporary/retained/.problem-packages.identity.final.json"
retained_root_final="$(capture_retained_volume "$retained_identity_final")"
[[ "$retained_root_final" == "$retained_root" ]] || die "Problem retained volume Mountpoint changed before publication"
cmp -s "$retained_identity" "$retained_identity_final" || die "Problem retained volume identity changed before publication"
rm -f -- "$retained_identity_final" "$retained_inspect"
retained_inventory_final="$temporary/retained/.problem-packages.inventory.final.json"
python3 "$script_dir/backup-manifest.py" inventory \
  --root "$retained_root" --output "$retained_inventory_final"
cmp -s "$temporary/retained/problem-packages.inventory.json" "$retained_inventory_final" || \
  die "Problem retained volume live tree changed before publication"
rm -f -- "$retained_inventory_final"
python3 "$script_dir/backup-manifest.py" verify --root "$temporary" --environment "$environment"
mv "$temporary" "$final"
trap - EXIT INT TERM HUP

if [[ -n "${OJOS_BACKUP_TEXTFILE_DIR:-}" ]]; then
  metric_tmp="$OJOS_BACKUP_TEXTFILE_DIR/ojos_backup.prom.$$"
  if ! {
    mkdir -p "$OJOS_BACKUP_TEXTFILE_DIR" &&
    {
      printf '# HELP ojos_backup_last_success_timestamp_seconds Last successful OJOS backup Unix timestamp.\n'
      printf '# TYPE ojos_backup_last_success_timestamp_seconds gauge\n'
      printf 'ojos_backup_last_success_timestamp_seconds{environment="%s"} %s\n' "$environment" "$(date -u +%s)"
    } >"$metric_tmp" &&
    mv "$metric_tmp" "$OJOS_BACKUP_TEXTFILE_DIR/ojos_backup.prom"
  }; then
    rm -f -- "$metric_tmp" 2>/dev/null || true
    echo "backup: WARNING: backup was published but the success metric could not be updated" >&2
  fi
fi
echo "backup: completed $final"
