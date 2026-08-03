#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "orchestrator-restore: $*" >&2; exit 1; }
for command_name in pg_restore sha256sum psql tar date; do
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
staged_artifacts="$artifact_parent/.${artifact_name}.restore.$$"
previous_artifacts="$artifact_parent/.${artifact_name}.before-restore.$(date -u +%Y%m%dT%H%M%SZ).$$"
[[ ! -e "$staged_artifacts" && ! -e "$previous_artifacts" ]] || \
  die "artifact restore staging path already exists"
mkdir -m 0700 "$staged_artifacts"
tar --no-same-owner -xzf "$restore_dir/orchestrator-artifacts.tar.gz" -C "$staged_artifacts"
had_previous=0
if [[ -e "$artifact_root" ]]; then
  [[ -d "$artifact_root" ]] || die "artifact restore target exists and is not a directory"
  mv "$artifact_root" "$previous_artifacts"
  had_previous=1
fi
mv "$staged_artifacts" "$artifact_root"

rollback_artifacts() {
  local rc=$?
  if [[ $rc -ne 0 ]]; then
    rm -rf -- "$artifact_root"
    if [[ "$had_previous" == "1" && -d "$previous_artifacts" ]]; then
      mv "$previous_artifacts" "$artifact_root"
    fi
  fi
  [[ ! -e "$staged_artifacts" ]] || rm -rf -- "$staged_artifacts"
  exit "$rc"
}
trap rollback_artifacts EXIT

pg_restore --clean --if-exists --no-owner --no-acl \
  --single-transaction --exit-on-error --dbname "$ORCHESTRATOR_DATABASE_URL" \
  "$restore_dir/orchestrator.dump"
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
trap - EXIT
if [[ "$had_previous" == "1" ]]; then
  echo "orchestrator-restore: previous artifact directory retained at $previous_artifacts"
fi
echo "orchestrator-restore: database and artifacts restored; run orchestrator-preflight.sh and start one control plane, then require /api/v1/healthz/ready=200 before reopening traffic"
