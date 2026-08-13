#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "restore: $*" >&2; exit 1; }
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
directory_is_empty() { [[ -d "$1" ]] && [[ -z "$(find "$1" -mindepth 1 -maxdepth 1 -print -quit)" ]]; }

load_env_file
for command_name in chown cmp cp find jq pg_restore psql python3 sha256sum stat tar; do need_cmd "$command_name"; done
restore_dir="${OJOS_RESTORE_DIR:-${1:-}}"
[[ -n "$restore_dir" && -d "$restore_dir" ]] || die "set OJOS_RESTORE_DIR or pass a backup directory"
restore_dir="$(cd "$restore_dir" && pwd -P)"
safe_directory "$restore_dir" "resolved OJOS_RESTORE_DIR"
environment="${OJOS_ENVIRONMENT:-production}"
[[ "$environment" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$ ]] || die "OJOS_ENVIRONMENT is invalid"
source_id="$(require_env OJOS_RESTORE_SOURCE_ID)"

python3 "$script_dir/backup-manifest.py" verify \
  --root "$restore_dir" --environment "$environment" --expected-source-id "$source_id"
if [[ "$(jq -r '.components.storage.local.included' "$restore_dir/manifest.json")" == "true" ]]; then
  python3 "$script_dir/backup-manifest.py" verify-tar \
    --archive "$restore_dir/storage/storage-root.tar.gz" \
    --expected-summary-json "$(jq -c '.components.storage.local.tree' "$restore_dir/manifest.json")"
fi
# The required retained-volume archive has its own exact inventory and stable
# Agent/Docker identity. Verify it before honoring VERIFY_ONLY or touching any
# target component.
python3 "$script_dir/backup-manifest.py" verify-tar \
  --archive "$restore_dir/retained/problem-packages.tar.gz" \
  --expected-summary-json "$(jq -c '.components.problem_retained_volume.tree' "$restore_dir/manifest.json")" >/dev/null
if [[ "${OJOS_RESTORE_VERIFY_ONLY:-0}" == "1" ]]; then
  echo "restore: backup verified; no target was changed"
  exit 0
fi

target_id="$(require_env OJOS_RESTORE_TARGET_ID)"
[[ "$target_id" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$ ]] || die "OJOS_RESTORE_TARGET_ID is invalid"
[[ "$target_id" != "$source_id" ]] || die "restore target must be a distinct clean environment; same-environment in-place restore is forbidden"
[[ "${OJOS_CONFIRM_RESTORE:-}" == "restore-$environment-clean-target-v1" ]] || \
  die "set OJOS_CONFIRM_RESTORE=restore-$environment-clean-target-v1"
[[ "${OJOS_CONFIRM_CLEAN_TARGET:-}" == "clean-target-v1" ]] || \
  die "set OJOS_CONFIRM_CLEAN_TARGET=clean-target-v1 after proving every target component is empty"
[[ -n "${OJOS_RESTORE_FENCE_TOKEN:-}" ]] || die "OJOS_RESTORE_FENCE_TOKEN is required"
[[ "${OJOS_RESTORE_FENCE_TOKEN}" != *$'\n'* && "${OJOS_RESTORE_FENCE_TOKEN}" != *$'\r'* ]] || \
  die "OJOS_RESTORE_FENCE_TOKEN must be one line"
verify_target_fence() {
  if [[ -n "${OJOS_RESTORE_FENCE_CHECK_COMMAND:-}" ]]; then
    validate_shell_command "$OJOS_RESTORE_FENCE_CHECK_COMMAND" "OJOS_RESTORE_FENCE_CHECK_COMMAND"
    OJOS_FENCE_TOKEN="$OJOS_RESTORE_FENCE_TOKEN" bash -Eeuo pipefail -c "$OJOS_RESTORE_FENCE_CHECK_COMMAND" || \
      die "external target fence check failed"
  elif [[ "${OJOS_RESTORE_ALLOW_DECLARED_FENCE:-0}" != "1" ]]; then
    die "set OJOS_RESTORE_FENCE_CHECK_COMMAND to verify the target fence; use OJOS_RESTORE_ALLOW_DECLARED_FENCE=1 only for an isolated drill"
  fi
}
verify_target_fence

cutover_command="${OJOS_RESTORE_CUTOVER_COMMAND:-}"
rollback_command="${OJOS_RESTORE_ROLLBACK_COMMAND:-}"
[[ -z "$cutover_command" && -z "$rollback_command" || -n "$cutover_command" && -n "$rollback_command" ]] || \
  die "OJOS_RESTORE_CUTOVER_COMMAND and OJOS_RESTORE_ROLLBACK_COMMAND must be configured together"
[[ -z "$cutover_command" || -n "${OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND:-}" ]] || \
  die "OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND is required when automatic cutover is enabled"
[[ -z "$cutover_command" || -n "${OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND:-}" ]] || \
  die "OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND is required when automatic cutover is enabled"
for command_variable in OJOS_RESTORE_CUTOVER_COMMAND OJOS_RESTORE_ROLLBACK_COMMAND \
  OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND \
  OJOS_RESTORE_COMPONENT_CHECK_COMMAND \
  OJOS_RESTORE_FAILED_TARGET_CLEANUP_COMMAND; do
  [[ -z "${!command_variable:-}" ]] || validate_shell_command "${!command_variable}" "$command_variable"
done
case "${OJOS_RESTORE_FAILPOINT:-}" in
  ""|after-databases|after-redis|after-storage|after-retained-volume|after-components) ;;
  *) die "unsupported OJOS_RESTORE_FAILPOINT" ;;
esac

work_root="${OJOS_RESTORE_WORK_ROOT:-$(dirname "$restore_dir")/.ojos-restore-work}"
safe_directory "$work_root" "OJOS_RESTORE_WORK_ROOT"
[[ -d "$work_root" && ! -L "$work_root" ]] || \
  die "OJOS_RESTORE_WORK_ROOT must be an existing real directory"
work_root="$(cd "$work_root" && pwd -P)"
safe_directory "$work_root" "resolved OJOS_RESTORE_WORK_ROOT"
paths_overlap "$restore_dir" "$work_root" && \
  die "restore backup and work root must not overlap"
umask 077
work_dir="$(mktemp -d "$work_root/ojos-restore.XXXXXXXX")"
[[ "$work_dir" == "$work_root/ojos-restore."* ]] || die "failed to create bounded restore work directory"
cutover_started=0
cleanup() {
  local rc=$? rollback_verified=0
  trap - EXIT INT TERM HUP
  if [[ $rc -eq 0 ]]; then
    [[ ! -d "$work_dir" ]] || rm -rf -- "$work_dir"
    exit 0
  fi
  if [[ $rc -ne 0 && "$cutover_started" == "1" ]]; then
    echo "restore: post-cutover failure; invoking the configured traffic rollback" >&2
    if OJOS_RESTORE_SOURCE_ID="$source_id" OJOS_RESTORE_TARGET_ID="$target_id" \
      bash -Eeuo pipefail -c "$rollback_command" && \
      OJOS_RESTORE_SOURCE_ID="$source_id" OJOS_RESTORE_TARGET_ID="$target_id" \
      bash -Eeuo pipefail -c "$OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND"; then
      rollback_verified=1
    else
      echo "restore: CRITICAL: traffic rollback could not be independently verified" >&2
    fi
  fi
  if [[ $rc -ne 0 && -n "${OJOS_RESTORE_FAILED_TARGET_CLEANUP_COMMAND:-}" && \
    ( "$cutover_started" == "0" || "$rollback_verified" == "1" ) ]]; then
    OJOS_RESTORE_SOURCE_ID="$source_id" OJOS_RESTORE_TARGET_ID="$target_id" \
      OJOS_RESTORE_WORK_DIR="$work_dir" bash -Eeuo pipefail -c "$OJOS_RESTORE_FAILED_TARGET_CLEANUP_COMMAND" || \
      echo "restore: WARNING: configured failed-target cleanup did not complete" >&2
  fi
  if [[ "$cutover_started" == "1" && "$rollback_verified" != "1" ]]; then
    echo "restore: CRITICAL: traffic state is unknown; target cleanup is forbidden; work evidence=$work_dir" >&2
  else
    echo "restore: failed target remains isolated; work evidence=$work_dir" >&2
  fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

evidence_dir="${OJOS_RESTORE_EVIDENCE_DIR:-}"
if [[ -n "$evidence_dir" ]]; then
  safe_directory "$evidence_dir" "OJOS_RESTORE_EVIDENCE_DIR"
  [[ -d "$evidence_dir" && ! -L "$evidence_dir" ]] || \
    die "OJOS_RESTORE_EVIDENCE_DIR must be an existing real directory"
  evidence_dir="$(cd "$evidence_dir" && pwd -P)"
  safe_directory "$evidence_dir" "resolved OJOS_RESTORE_EVIDENCE_DIR"
  paths_overlap "$restore_dir" "$evidence_dir" && \
    die "restore backup and evidence directory must not overlap"
  paths_overlap "$work_root" "$evidence_dir" && \
    die "restore work and evidence directories must not overlap"
  [[ ! -e "$evidence_dir/restore-manifest-identity.json" && \
     ! -e "$evidence_dir/backup-manifest.sha256" && \
     ! -e "$evidence_dir/components-verified.txt" && \
     ! -e "$evidence_dir/cutover-result.txt" ]] || \
    die "restore evidence files already exist; use a new run-specific evidence directory"
  printf '%s\n' "$(jq -c '{schema_version,created_at,environment,source_id,fence_id_sha256}' "$restore_dir/manifest.json")" \
    >"$evidence_dir/restore-manifest-identity.json"
  sha256sum "$restore_dir/manifest.json" >"$evidence_dir/backup-manifest.sha256"
fi

# Prove every retained-volume clean-target invariant before the first database
# mutation. The archive, inventory and identity are copied into the private
# work root and re-bound to the already verified manifest to close backup-dir
# TOCTOU windows.
retained_owner_instance_id="$(require_env OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID)"
retained_volume_name="$(require_env OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_NAME)"
retained_target_id="$(require_env OJOS_RESTORE_RETAINED_VOLUME_TARGET_ID)"
[[ "$retained_target_id" == "$target_id" ]] || \
  die "OJOS_RESTORE_RETAINED_VOLUME_TARGET_ID must exactly match OJOS_RESTORE_TARGET_ID"
retained_archive="$work_dir/problem-packages.tar.gz"
retained_inventory="$work_dir/problem-packages.inventory.json"
retained_backup_identity="$work_dir/problem-packages.backup.identity.json"
cp -- "$restore_dir/retained/problem-packages.tar.gz" "$retained_archive"
cp -- "$restore_dir/retained/problem-packages.inventory.json" "$retained_inventory"
cp -- "$restore_dir/retained/problem-packages.identity.json" "$retained_backup_identity"
manifest_payload_sha256() {
  jq -er --arg path "$1" \
    '.payload_files[] | select(.path == $path) | .sha256' "$restore_dir/manifest.json"
}
[[ "$(sha256sum "$retained_archive" | awk '{print $1}')" == \
   "$(manifest_payload_sha256 retained/problem-packages.tar.gz)" ]] || \
  die "staged retained-volume archive digest does not match the verified manifest"
[[ "$(sha256sum "$retained_inventory" | awk '{print $1}')" == \
   "$(manifest_payload_sha256 retained/problem-packages.inventory.json)" ]] || \
  die "staged retained-volume inventory digest does not match the verified manifest"
[[ "$(sha256sum "$retained_backup_identity" | awk '{print $1}')" == \
   "$(manifest_payload_sha256 retained/problem-packages.identity.json)" ]] || \
  die "staged retained-volume identity digest does not match the verified manifest"
backup_retained_owner="$(jq -er '.labels["ojos.owner_instance_id"]' "$retained_backup_identity")"
[[ "$retained_owner_instance_id" == "$backup_retained_owner" ]] || \
  die "target retained-volume owner identity does not match the backed-up stable service instance"

retained_inspect="$work_dir/problem-retained-volume.target.inspect.json"
inspect_target_retained_volume() {
  local output="$1" phase="${2:-initial}" running
  if [[ -n "${OJOS_RESTORE_RETAINED_VOLUME_INSPECT_FILE:-}" ]]; then
    [[ "${OJOS_RESTORE_ALLOW_INSPECT_FIXTURE:-0}" == "1" && \
       "${OJOS_RESTORE_ALLOW_DECLARED_FENCE:-0}" == "1" ]] || \
      die "retained-volume inspect fixtures are allowed only in an isolated declared-fence drill"
    [[ -f "$OJOS_RESTORE_RETAINED_VOLUME_INSPECT_FILE" && \
       ! -L "$OJOS_RESTORE_RETAINED_VOLUME_INSPECT_FILE" ]] || \
      die "OJOS_RESTORE_RETAINED_VOLUME_INSPECT_FILE must be a regular file, not a link"
    cp -- "$OJOS_RESTORE_RETAINED_VOLUME_INSPECT_FILE" "$output"
  else
    need_cmd docker
    docker volume inspect "$retained_volume_name" >"$output" || \
      die "cannot inspect clean target Problem retained volume $retained_volume_name"
    running="$(docker ps --quiet --filter "volume=$retained_volume_name" --filter status=running)"
    if [[ -n "$running" ]]; then
      [[ "$phase" != "after" ]] || \
        die "target Problem retained volume acquired a running mount during restore"
      die "target Problem retained volume is mounted by a running container"
    fi
  fi
}
inspect_target_retained_volume "$retained_inspect"
retained_target_identity="$work_dir/problem-packages.target.identity.json"
retained_root="$(python3 "$script_dir/retained-volume.py" \
  --inspect "$retained_inspect" \
  --owner-instance-id "$retained_owner_instance_id" \
  --root "${OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_ROOT:-$(jq -r '.[0].Mountpoint // empty' "$retained_inspect")}" \
  --output "$retained_target_identity" --print-mountpoint)"
[[ "$(jq -r '.[0].Name // empty' "$retained_inspect")" == "$retained_volume_name" ]] || \
  die "OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_NAME does not match Docker inspect"
cmp -s "$retained_backup_identity" "$retained_target_identity" || \
  die "clean target retained volume does not have the backed-up stable Agent identity"
safe_directory "$retained_root" "target Problem retained volume Mountpoint"
directory_is_empty "$retained_root" || \
  die "target Problem retained volume is not empty; clean-target restore refuses to merge trees"
paths_overlap "$restore_dir" "$retained_root" && die "restore backup and retained volume must not overlap"
paths_overlap "$work_root" "$retained_root" && die "restore work and retained volume must not overlap"
[[ -z "$evidence_dir" ]] || ! paths_overlap "$evidence_dir" "$retained_root" || \
  die "restore evidence and retained volume must not overlap"
retained_stage="$work_dir/problem-retained-volume.stage"
mkdir -m 0700 "$retained_stage"
tar --no-same-owner --no-same-permissions -xzf "$retained_archive" -C "$retained_stage"
python3 "$script_dir/backup-manifest.py" verify-inventory \
  --root "$retained_stage" --inventory "$retained_inventory" >/dev/null
verify_target_fence

declare -a db_specs=(
  "orchestrator:ORCHESTRATOR_DATABASE_URL:orchestrator_schema_migrations"
  "auth:AUTH_DATABASE_URL:users"
  "problem:PROBLEM_DATABASE_URL:problems"
  "judge:JUDGE_DATABASE_URL:submissions"
  "user:USER_DATABASE_URL:user_profiles"
)
for spec in "${db_specs[@]}"; do
  name="${spec%%:*}"
  remainder="${spec#*:}"
  var="${remainder%%:*}"
  required_table="${remainder##*:}"
  url="$(require_env "$var")"
  existing_tables="$(psql "$url" -v ON_ERROR_STOP=1 -Atc \
    "SELECT count(*) FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog','information_schema')")"
  [[ "$existing_tables" == "0" ]] || die "$name target database is not empty"
  echo "restore: restoring $name into the fenced clean target"
  pg_restore --no-owner --no-acl --single-transaction --exit-on-error \
    --dbname "$url" "$restore_dir/postgres/$name.dump"
  [[ "$(psql "$url" -v ON_ERROR_STOP=1 -Atc "SELECT to_regclass('public.$required_table') IS NOT NULL")" == "t" ]] || \
    die "$name restore is missing required table $required_table"
done

[[ "${OJOS_RESTORE_FAILPOINT:-}" != "after-databases" ]] || die "injected failure after databases"

redis_included="$(jq -r '.components.redis.included' "$restore_dir/manifest.json")"
if [[ "$redis_included" == "true" ]]; then
  need_cmd redis-check-rdb
  redis_rdb_path="$(require_env OJOS_REDIS_RDB_PATH)"
  [[ "$redis_rdb_path" == /* && "$redis_rdb_path" != "/" ]] || die "OJOS_REDIS_RDB_PATH must be an absolute file path"
  [[ "$redis_rdb_path" != *$'\n'* && "$redis_rdb_path" != *$'\r'* ]] || \
    die "OJOS_REDIS_RDB_PATH must be one line"
  redis_owner="$(require_env OJOS_RESTORE_REDIS_OWNER)"
  [[ "$redis_owner" =~ ^[A-Za-z_][A-Za-z0-9_.-]*(:[A-Za-z_][A-Za-z0-9_.-]*)?$ || \
     "$redis_owner" =~ ^[0-9]+(:[0-9]+)?$ ]] || die "OJOS_RESTORE_REDIS_OWNER must be USER[:GROUP] or UID[:GID]"
  [[ ! -e "$redis_rdb_path" ]] || die "Redis target RDB already exists; target is not clean"
  redis_parent="$(dirname "$redis_rdb_path")"
  safe_directory "$redis_parent" "Redis target parent"
  [[ -d "$redis_parent" && ! -L "$redis_parent" ]] || \
    die "Redis target parent must be an existing real directory"
  redis_parent="$(cd "$redis_parent" && pwd -P)"
  safe_directory "$redis_parent" "resolved Redis target parent"
  paths_overlap "$restore_dir" "$redis_parent" && die "restore backup and Redis target must not overlap"
  paths_overlap "$work_root" "$redis_parent" && die "restore work and Redis target must not overlap"
  [[ -z "$evidence_dir" ]] || ! paths_overlap "$evidence_dir" "$redis_parent" || \
    die "restore evidence and Redis target must not overlap"
  redis_stage="$redis_parent/.$(basename "$redis_rdb_path").restore.$$"
  [[ ! -e "$redis_stage" ]] || die "Redis staging path already exists"
  install -m 0600 "$restore_dir/redis/dump.rdb" "$redis_stage"
  chown "$redis_owner" "$redis_stage"
  redis-check-rdb "$redis_stage" >"$work_dir/redis-check-rdb.txt"
  mv "$redis_stage" "$redis_rdb_path"
  [[ "$(stat -c '%a' "$redis_rdb_path")" == "600" ]] || die "restored Redis RDB mode is not 0600"
fi
[[ "${OJOS_RESTORE_FAILPOINT:-}" != "after-redis" ]] || die "injected failure after Redis"

local_included="$(jq -r '.components.storage.local.included' "$restore_dir/manifest.json")"
if [[ "$local_included" == "true" ]]; then
  storage_root="$(require_env OJOS_STORAGE_ROOT)"
  safe_directory "$storage_root" "OJOS_STORAGE_ROOT"
  storage_owner="$(require_env OJOS_RESTORE_STORAGE_OWNER)"
  [[ "$storage_owner" =~ ^[A-Za-z_][A-Za-z0-9_.-]*(:[A-Za-z_][A-Za-z0-9_.-]*)?$ || \
     "$storage_owner" =~ ^[0-9]+(:[0-9]+)?$ ]] || die "OJOS_RESTORE_STORAGE_OWNER must be USER[:GROUP] or UID[:GID]"
  [[ ! -e "$storage_root" ]] || die "local storage target already exists; clean restore requires an absent target"
  storage_parent="$(dirname "$storage_root")"
  storage_name="$(basename "$storage_root")"
  [[ -d "$storage_parent" && ! -L "$storage_parent" ]] || \
    die "local storage target parent must be an existing real directory"
  storage_parent="$(cd "$storage_parent" && pwd -P)"
  safe_directory "$storage_parent" "resolved local storage target parent"
  paths_overlap "$restore_dir" "$storage_parent" && die "restore backup and local storage target must not overlap"
  paths_overlap "$work_root" "$storage_parent" && die "restore work and local storage target must not overlap"
  [[ -z "$evidence_dir" ]] || ! paths_overlap "$evidence_dir" "$storage_parent" || \
    die "restore evidence and local storage target must not overlap"
  storage_stage="$storage_parent/.${storage_name}.restore.$$"
  [[ ! -e "$storage_stage" ]] || die "local storage staging path already exists"
  mkdir -m 0700 "$storage_stage"
  tar --no-same-owner --no-same-permissions -xzf "$restore_dir/storage/storage-root.tar.gz" -C "$storage_stage"
  python3 "$script_dir/backup-manifest.py" verify-tree \
    --root "$storage_stage" \
    --expected-summary-json "$(jq -c '.components.storage.local.tree' "$restore_dir/manifest.json")"
  chown -R "$storage_owner" "$storage_stage"
  chmod 0700 "$storage_stage"
  mv "$storage_stage" "$storage_root"
fi
[[ "${OJOS_RESTORE_FAILPOINT:-}" != "after-storage" ]] || die "injected failure after local storage"

# Re-prove the clean target immediately before the first retained-volume write;
# database/local restore time must not allow a writer or foreign remount in.
retained_inspect_before_copy="$work_dir/problem-retained-volume.before-copy.inspect.json"
inspect_target_retained_volume "$retained_inspect_before_copy" before-copy
retained_identity_before_copy="$work_dir/problem-packages.target.identity.before-copy.json"
retained_root_before_copy="$(python3 "$script_dir/retained-volume.py" \
  --inspect "$retained_inspect_before_copy" \
  --owner-instance-id "$retained_owner_instance_id" \
  --root "$retained_root" --output "$retained_identity_before_copy" --print-mountpoint)"
[[ "$retained_root_before_copy" == "$retained_root" ]] || \
  die "target Problem retained volume Mountpoint changed before restore copy"
cmp -s "$retained_target_identity" "$retained_identity_before_copy" || \
  die "target Problem retained volume identity changed before restore copy"
directory_is_empty "$retained_root" || \
  die "target Problem retained volume became non-empty before restore copy"
verify_target_fence
cp -a -- "$retained_stage/." "$retained_root/"
retained_owner="$(require_env OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER)"
[[ "$retained_owner" =~ ^[A-Za-z_][A-Za-z0-9_.-]*(:[A-Za-z_][A-Za-z0-9_.-]*)?$ || \
   "$retained_owner" =~ ^[0-9]+(:[0-9]+)?$ ]] || \
  die "OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER must be USER[:GROUP] or UID[:GID]"
chown -R "$retained_owner" "$retained_root"
python3 "$script_dir/backup-manifest.py" verify-inventory \
  --root "$retained_root" --inventory "$retained_inventory" >/dev/null
# Re-inspect after the final copy. A volume replacement/remount or a writer
# restart during restore invalidates the clean-target claim.
inspect_target_retained_volume "$retained_inspect.after" after
retained_identity_after="$work_dir/problem-packages.target.identity.after.json"
retained_root_after="$(python3 "$script_dir/retained-volume.py" \
  --inspect "$retained_inspect.after" \
  --owner-instance-id "$retained_owner_instance_id" \
  --root "$retained_root" --output "$retained_identity_after" --print-mountpoint)"
[[ "$retained_root_after" == "$retained_root" ]] || \
  die "target Problem retained volume Mountpoint changed during restore"
cmp -s "$retained_target_identity" "$retained_identity_after" || \
  die "target Problem retained volume identity changed during restore"
python3 "$script_dir/backup-manifest.py" verify-inventory \
  --root "$retained_root" --inventory "$retained_inventory" >/dev/null
verify_target_fence
[[ "${OJOS_RESTORE_FAILPOINT:-}" != "after-retained-volume" ]] || \
  die "injected failure after Problem retained volume"

minio_included="$(jq -r '.components.storage.minio.included' "$restore_dir/manifest.json")"
if [[ "$minio_included" == "true" ]]; then
  need_cmd mc
  endpoint="${OJOS_RESTORE_MINIO_ENDPOINT:-${MINIO_ENDPOINT:-}}"
  access_key="${MINIO_ACCESS_KEY:-${MINIO_ROOT_USER:-}}"
  secret_key="${MINIO_SECRET_KEY:-${MINIO_ROOT_PASSWORD:-}}"
  [[ -n "$endpoint" && -n "$access_key" && -n "$secret_key" ]] || die "clean MinIO target endpoint and credentials are required"
  minio_target_id="$(require_env OJOS_RESTORE_MINIO_TARGET_ID)"
  [[ "$minio_target_id" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$ ]] || \
    die "OJOS_RESTORE_MINIO_TARGET_ID is invalid"
  [[ "$minio_target_id" == "$target_id" ]] || \
    die "OJOS_RESTORE_MINIO_TARGET_ID must exactly match OJOS_RESTORE_TARGET_ID"
  if [[ "$endpoint" != http://* && "$endpoint" != https://* ]]; then
    [[ "${MINIO_USE_SSL:-false}" == "true" ]] && endpoint="https://$endpoint" || endpoint="http://$endpoint"
  fi
  alias_name="${OJOS_RESTORE_MINIO_ALIAS:-ojos-restore}"
  mc alias set "$alias_name" "$endpoint" "$access_key" "$secret_key" >/dev/null
  mapfile -t bucket_list < <(jq -r '.components.storage.minio.buckets[].name' "$restore_dir/manifest.json")
  created_minio_buckets="$work_dir/created-minio-buckets.txt"
  : >"$created_minio_buckets"
  for bucket in "${bucket_list[@]}"; do
    if mc stat "$alias_name/$bucket" >/dev/null 2>&1; then
      die "MinIO target bucket $bucket already exists; target is not clean"
    fi
  done
  for bucket in "${bucket_list[@]}"; do
    mc mb "$alias_name/$bucket" >/dev/null
    printf '%s\n' "$bucket" >>"$created_minio_buckets"
    mc mirror --overwrite "$restore_dir/storage/minio/$bucket" "$alias_name/$bucket"
    verify_bucket="$work_dir/minio/$bucket"
    mkdir -p "$verify_bucket"
    mc mirror --overwrite "$alias_name/$bucket" "$verify_bucket"
    python3 "$script_dir/backup-manifest.py" verify-tree \
      --root "$verify_bucket" \
      --expected-summary-json "$(jq -c --arg bucket "$bucket" '.components.storage.minio.buckets[] | select(.name == $bucket) | .tree' "$restore_dir/manifest.json")"
  done
fi

if [[ -n "${OJOS_RESTORE_COMPONENT_CHECK_COMMAND:-}" ]]; then
  OJOS_RESTORE_SOURCE_ID="$source_id" OJOS_RESTORE_TARGET_ID="$target_id" \
    bash -Eeuo pipefail -c "$OJOS_RESTORE_COMPONENT_CHECK_COMMAND" || die "restored component verification failed"
fi

# MinIO mirroring and the operator-supplied component check can be long-running.
# Re-prove the retained identity, exact tree and absence of a writer after both,
# immediately before the final fence and optional traffic cutover.
retained_inspect_final="$work_dir/problem-retained-volume.final.inspect.json"
inspect_target_retained_volume "$retained_inspect_final" final
retained_identity_final="$work_dir/problem-packages.target.identity.final.json"
retained_root_final="$(python3 "$script_dir/retained-volume.py" \
  --inspect "$retained_inspect_final" \
  --owner-instance-id "$retained_owner_instance_id" \
  --root "$retained_root" --output "$retained_identity_final" --print-mountpoint)"
[[ "$retained_root_final" == "$retained_root" ]] || \
  die "target Problem retained volume Mountpoint changed before cutover"
cmp -s "$retained_target_identity" "$retained_identity_final" || \
  die "target Problem retained volume identity changed before cutover"
python3 "$script_dir/backup-manifest.py" verify-inventory \
  --root "$retained_root" --inventory "$retained_inventory" >/dev/null

[[ "${OJOS_RESTORE_FAILPOINT:-}" != "after-components" ]] || die "injected failure after component verification"
# Verify that no writer entered while the target was being restored. This is
# deliberately before any optional traffic cutover.
verify_target_fence
if [[ -n "$evidence_dir" ]]; then
  {
    printf 'source_id=%s\n' "$source_id"
    printf 'target_id=%s\n' "$target_id"
    printf 'components=verified\n'
    printf 'problem_retained_volume=identity-inventory-tree-verified\n'
    printf 'traffic_changed=no\n'
  } >"$evidence_dir/components-verified.txt"
fi

if [[ -z "$cutover_command" ]]; then
  echo "restore: clean target restored and verified; traffic was not changed"
  echo "restore: configure paired cutover/rollback commands and a post-cutover check for a controlled promotion"
else
  cutover_started=1
  OJOS_RESTORE_SOURCE_ID="$source_id" OJOS_RESTORE_TARGET_ID="$target_id" \
    bash -Eeuo pipefail -c "$cutover_command"
  OJOS_RESTORE_SOURCE_ID="$source_id" OJOS_RESTORE_TARGET_ID="$target_id" \
    bash -Eeuo pipefail -c "$OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND"
  if [[ -n "$evidence_dir" ]]; then
    printf 'traffic_changed=yes\npost_cutover_check=passed\n' >"$evidence_dir/cutover-result.txt"
  fi
  cutover_started=0
  echo "restore: clean target verified and traffic cutover passed"
fi
