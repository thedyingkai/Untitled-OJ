#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "orchestrator-restore: $*" >&2; exit 1; }
for command_name in awk date find grep pg_dump pg_restore psql sed sha256sum tar wc; do
  command -v "$command_name" >/dev/null 2>&1 || die "$command_name is required"
done
restore_dir="${ORCHESTRATOR_RESTORE_DIR:-${1:-}}"
[[ -n "$restore_dir" && -d "$restore_dir" ]] || die "set ORCHESTRATOR_RESTORE_DIR or pass a backup directory"
[[ -n "${ORCHESTRATOR_DATABASE_URL:-}" ]] || die "ORCHESTRATOR_DATABASE_URL is required"
artifact_root="${ORCHESTRATOR_ARTIFACT_DIR:-}"
[[ -n "$artifact_root" && "$artifact_root" == /* ]] || \
  die "ORCHESTRATOR_ARTIFACT_DIR must be an absolute dedicated path"
[[ "$artifact_root" != "/" && "$artifact_root" != "${HOME:-}" ]] || \
  die "artifact root must be a dedicated directory"
[[ "${ORCHESTRATOR_CONFIRM_RESTORE:-}" == "restore-orchestrator-v1" ]] || \
  die "set ORCHESTRATOR_CONFIRM_RESTORE=restore-orchestrator-v1"
[[ -f "$restore_dir/SHA256SUMS" && -f "$restore_dir/orchestrator.dump" && \
   -f "$restore_dir/orchestrator-artifacts.tar.gz" && -f "$restore_dir/manifest.json" ]] || \
  die "backup is incomplete"
(cd "$restore_dir" && sha256sum -c SHA256SUMS)
grep -Eq '"schema_version"[[:space:]]*:[[:space:]]*1([[:space:]]*[,}])' "$restore_dir/manifest.json" && \
  grep -Eq '"product"[[:space:]]*:[[:space:]]*"OJOS Orchestrator"' "$restore_dir/manifest.json" && \
  grep -Eq '"consistency"[[:space:]]*:[[:space:]]*"control-plane-quiesced"' "$restore_dir/manifest.json" && \
  grep -Eq '"database"[[:space:]]*:[[:space:]]*"orchestrator"' "$restore_dir/manifest.json" && \
  grep -Eq '"artifact_archive"[[:space:]]*:[[:space:]]*"orchestrator-artifacts.tar.gz"' "$restore_dir/manifest.json" && \
  grep -Eq '"fence_id_sha256"[[:space:]]*:[[:space:]]*"[0-9a-f]{64}"' "$restore_dir/manifest.json" || \
  die "backup manifest identity or fence evidence is invalid"

if [[ -n "${ORCHESTRATOR_HEALTH_URL:-}" ]] && command -v curl >/dev/null 2>&1; then
  if curl -fsS --max-time 2 "$ORCHESTRATOR_HEALTH_URL/api/v1/healthz/live" >/dev/null 2>&1; then
    die "daemon is still live; stop it before restore"
  fi
fi

artifact_parent="$(dirname "$artifact_root")"
artifact_name="$(basename "$artifact_root")"
mkdir -p "$artifact_parent"
artifact_parent="$(cd "$artifact_parent" && pwd -P)"
artifact_root="$artifact_parent/$artifact_name"
restore_dir="$(cd "$restore_dir" && pwd -P)"
case "$artifact_root/" in "$restore_dir/"*) die "artifact target must not be inside the backup directory" ;; esac
case "$restore_dir/" in "$artifact_root/"*) die "backup directory must not be inside the artifact target" ;; esac
staged_artifacts="$artifact_parent/.${artifact_name}.restore.$$"
previous_artifacts="$artifact_parent/.${artifact_name}.before-restore.$(date -u +%Y%m%dT%H%M%SZ).$$"
previous_database="$artifact_parent/.${artifact_name}.database-before-restore.$(date -u +%Y%m%dT%H%M%SZ).$$.dump"
previous_database_list="$previous_database.list"
[[ ! -e "$staged_artifacts" && ! -e "$previous_artifacts" && \
   ! -e "$previous_database" && ! -e "$previous_database_list" ]] || \
  die "artifact restore staging path already exists"
mkdir -m 0700 "$staged_artifacts"
cleanup_before_switch() {
  local rc=$?
  trap - EXIT INT TERM HUP
  [[ ! -e "$staged_artifacts" ]] || rm -rf -- "$staged_artifacts"
  [[ ! -e "$previous_database" ]] || rm -f -- "$previous_database"
  [[ ! -e "$previous_database_list" ]] || rm -f -- "$previous_database_list"
  exit "$rc"
}
trap cleanup_before_switch EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
tar --no-same-owner -xzf "$restore_dir/orchestrator-artifacts.tar.gz" -C "$staged_artifacts"
expected_artifact_files="$(sed -n 's/^[[:space:]]*"artifact_files":[[:space:]]*\([0-9][0-9]*\),*[[:space:]]*$/\1/p' "$restore_dir/manifest.json")"
expected_artifact_bytes="$(sed -n 's/^[[:space:]]*"artifact_bytes":[[:space:]]*\([0-9][0-9]*\),*[[:space:]]*$/\1/p' "$restore_dir/manifest.json")"
[[ "$expected_artifact_files" =~ ^[0-9]+$ && "$expected_artifact_bytes" =~ ^[0-9]+$ ]] || \
  die "backup manifest artifact inventory is invalid"
actual_artifact_files="$(find "$staged_artifacts" -type f | wc -l | tr -d ' ')"
actual_artifact_bytes="$(find "$staged_artifacts" -type f -printf '%s\n' | awk '{ total += $1 } END { print total + 0 }')"
[[ "$actual_artifact_files" == "$expected_artifact_files" && \
   "$actual_artifact_bytes" == "$expected_artifact_bytes" ]] || \
  die "extracted artifact inventory does not match the manifest"

# Capture a symmetric database fallback before either resource is switched.
pg_dump --format=custom --compress=9 --no-owner --no-acl \
  --file "$previous_database" "$ORCHESTRATOR_DATABASE_URL"
pg_restore --list "$previous_database" >"$previous_database_list"
trap - EXIT INT TERM HUP
had_previous=0
rollback_artifacts() {
  local rc=$? database_rollback_ok=1 artifact_switched_now="${artifact_switched:-0}"
  trap - EXIT INT TERM HUP
  if [[ $rc -ne 0 ]]; then
    if [[ "$database_committed" == "1" ]]; then
      echo "orchestrator-restore: restoring the pre-restore database snapshot" >&2
      pg_restore --clean --if-exists --no-owner --no-acl \
        --single-transaction --exit-on-error --dbname "$ORCHESTRATOR_DATABASE_URL" \
        "$previous_database" || database_rollback_ok=0
    fi
    if [[ "$artifact_switched_now" == "1" ]]; then
      [[ ! -e "$artifact_root" ]] || rm -rf -- "$artifact_root"
      if [[ "$had_previous" == "1" && -d "$previous_artifacts" ]]; then
        mv "$previous_artifacts" "$artifact_root"
      fi
    fi
    [[ "$database_rollback_ok" == "1" ]] || \
      echo "orchestrator-restore: CRITICAL: database rollback failed; keep traffic closed" >&2
  fi
  [[ ! -e "$staged_artifacts" ]] || rm -rf -- "$staged_artifacts"
  exit "$rc"
}
trap rollback_artifacts EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
database_committed=0
artifact_switched=0
if [[ -e "$artifact_root" ]]; then
  [[ -d "$artifact_root" ]] || die "artifact restore target exists and is not a directory"
  mv "$artifact_root" "$previous_artifacts"
  had_previous=1
fi
mv "$staged_artifacts" "$artifact_root"
artifact_switched=1

pg_restore --clean --if-exists --no-owner --no-acl \
  --single-transaction --exit-on-error --dbname "$ORCHESTRATOR_DATABASE_URL" \
  "$restore_dir/orchestrator.dump"
database_committed=1
missing_tables="$(psql "$ORCHESTRATOR_DATABASE_URL" -v ON_ERROR_STOP=1 -At <<'SQL'
WITH required(table_name) AS (
  VALUES
    ('orchestrator_schema_migrations'),
    ('orchestrator_records'),
    ('orchestrator_operation_logs_v2'),
    ('orchestrator_state'),
    ('orchestrator_jobs'),
    ('orchestrator_job_events'),
    ('orchestrator_topology_revisions'),
    ('orchestrator_topology_heads'),
    ('orchestrator_topology_status'),
    ('orchestrator_runtime_instances'),
    ('orchestrator_idempotency'),
    ('orchestrator_durable_operations'),
    ('orchestrator_audit_log'),
    ('orchestrator_node_enrollment_codes'),
    ('orchestrator_node_certificates'),
    ('orchestrator_legacy_imports')
)
SELECT required.table_name
FROM required
LEFT JOIN information_schema.tables actual
  ON actual.table_schema = 'public' AND actual.table_name = required.table_name
WHERE actual.table_name IS NULL
ORDER BY required.table_name;
SQL
)"
[[ -z "$missing_tables" ]] || die "restored schema is missing required tables: ${missing_tables//$'\n'/, }"
trap - EXIT INT TERM HUP
if [[ "$had_previous" == "1" ]]; then
  echo "orchestrator-restore: previous artifact directory retained at $previous_artifacts"
fi
echo "orchestrator-restore: pre-restore database retained at $previous_database"
echo "orchestrator-restore: database and artifacts restored; run orchestrator-preflight.sh and start one control plane, then require /api/v1/healthz/ready=200 before reopening traffic"
