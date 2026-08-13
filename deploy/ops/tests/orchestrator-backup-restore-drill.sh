#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "orchestrator-backup-restore-drill: $*" >&2; exit 1; }
for command_name in psql pg_dump pg_restore sha256sum tar find grep; do
  command -v "$command_name" >/dev/null 2>&1 || die "$command_name is required"
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
database_url="${ORCHESTRATOR_DRILL_DATABASE_URL:-}"
[[ "$database_url" =~ /ojos_orchestrator_backup_restore_drill([?].*)?$ ]] || \
  die "ORCHESTRATOR_DRILL_DATABASE_URL must target the dedicated ojos_orchestrator_backup_restore_drill database"

evidence_root="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/orchestrator-backup-restore-drill}"
mkdir -p "$evidence_root"
evidence_root="$(cd "$evidence_root" && pwd -P)"
[[ "$evidence_root" != "/" && "$evidence_root" != "${HOME:-}" ]] || \
  die "evidence root must be a dedicated directory"

stamp="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"
artifact_root="$evidence_root/live-artifacts-$stamp"
backup_root="$evidence_root/backups"
mkdir -p "$artifact_root" "$backup_root"

# This drill proves the backup/restore mechanics against a dedicated real
# PostgreSQL database. Full column/constraint/checksum compatibility is covered
# independently by the TLS PostgreSQL storage contract and daemon readiness.
psql "$database_url" -v ON_ERROR_STOP=1 <<'SQL'
DO $drill$
DECLARE
  table_name text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'orchestrator_schema_migrations',
    'orchestrator_records',
    'orchestrator_operation_logs_v2',
    'orchestrator_state',
    'orchestrator_jobs',
    'orchestrator_job_events',
    'orchestrator_topology_revisions',
    'orchestrator_topology_heads',
    'orchestrator_topology_status',
    'orchestrator_runtime_instances',
    'orchestrator_idempotency',
    'orchestrator_durable_operations',
    'orchestrator_audit_log',
    'orchestrator_node_enrollment_codes',
    'orchestrator_node_certificates',
    'orchestrator_legacy_imports'
  ]
  LOOP
    EXECUTE format('CREATE TABLE IF NOT EXISTS %I (drill_key text PRIMARY KEY)', table_name);
  END LOOP;
END
$drill$;
CREATE TABLE IF NOT EXISTS orchestrator_restore_probe (
  probe_key text PRIMARY KEY,
  probe_value text NOT NULL
);
INSERT INTO orchestrator_restore_probe(probe_key, probe_value)
VALUES ('backup-restore', 'before-backup')
ON CONFLICT (probe_key) DO UPDATE SET probe_value = EXCLUDED.probe_value;
SQL

printf '%s\n' 'before-backup artifact' >"$artifact_root/probe.txt"

ORCHESTRATOR_DATABASE_URL="$database_url" \
ORCHESTRATOR_ARTIFACT_DIR="$artifact_root" \
ORCHESTRATOR_BACKUP_DIR="$backup_root" \
ORCHESTRATOR_BACKUP_STAMP="$stamp" \
ORCHESTRATOR_HEALTH_URL= \
ORCHESTRATOR_CONFIRM_QUIESCED_BACKUP=backup-orchestrator-v1 \
ORCHESTRATOR_BACKUP_FENCE_TOKEN="isolated-drill-$stamp" \
ORCHESTRATOR_BACKUP_ALLOW_DECLARED_FENCE=1 \
  bash "$repo_root/deploy/ops/orchestrator-backup.sh"

psql "$database_url" -v ON_ERROR_STOP=1 -c \
  "UPDATE orchestrator_restore_probe SET probe_value = 'after-backup' WHERE probe_key = 'backup-restore'"
printf '%s\n' 'after-backup artifact' >"$artifact_root/probe.txt"

ORCHESTRATOR_DATABASE_URL="$database_url" \
ORCHESTRATOR_ARTIFACT_DIR="$artifact_root" \
ORCHESTRATOR_RESTORE_DIR="$backup_root/$stamp" \
ORCHESTRATOR_HEALTH_URL= \
ORCHESTRATOR_CONFIRM_RESTORE=restore-orchestrator-v1 \
  bash "$repo_root/deploy/ops/orchestrator-restore.sh"

[[ "$(psql "$database_url" -Atc "SELECT probe_value FROM orchestrator_restore_probe WHERE probe_key = 'backup-restore'")" == "before-backup" ]] || \
  die "database probe was not restored"
grep -qx 'before-backup artifact' "$artifact_root/probe.txt" || \
  die "artifact probe was not restored"

mapfile -t previous_artifacts < <(
  find "$evidence_root" -mindepth 1 -maxdepth 1 -type d \
    -name ".live-artifacts-$stamp.before-restore.*" -print
)
[[ "${#previous_artifacts[@]}" -eq 1 ]] || \
  die "restore must retain exactly one previous artifact directory"
grep -qx 'after-backup artifact' "${previous_artifacts[0]}/probe.txt" || \
  die "previous artifact directory does not contain the pre-restore state"
mapfile -t previous_databases < <(
  find "$evidence_root" -mindepth 1 -maxdepth 1 -type f \
    -name ".live-artifacts-$stamp.database-before-restore.*.dump" -print
)
[[ "${#previous_databases[@]}" -eq 1 ]] || \
  die "restore must retain exactly one pre-restore database snapshot"
pg_restore --list "${previous_databases[0]}" >/dev/null

{
  printf 'commit=%s\n' "${GITHUB_SHA:-local}"
  printf 'database_probe=restored\n'
  printf 'artifact_probe=restored\n'
  printf 'previous_artifacts=%s\n' "${previous_artifacts[0]}"
  printf 'previous_database=%s\n' "${previous_databases[0]}"
} >"$evidence_root/result.txt"

echo "orchestrator-backup-restore-drill: passed; evidence=$evidence_root"
