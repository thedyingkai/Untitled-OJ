#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "orchestrator-backup: $*" >&2; exit 1; }
for command_name in pg_dump pg_restore sha256sum date find tar; do
  command -v "$command_name" >/dev/null 2>&1 || die "$command_name is required"
done
[[ -n "${ORCHESTRATOR_DATABASE_URL:-}" ]] || die "ORCHESTRATOR_DATABASE_URL is required"
[[ "${ORCHESTRATOR_CONFIRM_QUIESCED_BACKUP:-}" == "backup-orchestrator-v1" ]] || \
  die "stop or drain the daemon, then set ORCHESTRATOR_CONFIRM_QUIESCED_BACKUP=backup-orchestrator-v1"
[[ -n "${ORCHESTRATOR_BACKUP_FENCE_TOKEN:-}" ]] || \
  die "ORCHESTRATOR_BACKUP_FENCE_TOKEN is required"
if [[ -n "${ORCHESTRATOR_BACKUP_FENCE_CHECK_COMMAND:-}" ]]; then
  ORCHESTRATOR_FENCE_TOKEN="$ORCHESTRATOR_BACKUP_FENCE_TOKEN" \
    bash -Eeuo pipefail -c "$ORCHESTRATOR_BACKUP_FENCE_CHECK_COMMAND" || \
    die "external control-plane write fence check failed"
elif [[ "${ORCHESTRATOR_BACKUP_ALLOW_DECLARED_FENCE:-0}" != "1" ]]; then
  die "ORCHESTRATOR_BACKUP_FENCE_CHECK_COMMAND is required; use ORCHESTRATOR_BACKUP_ALLOW_DECLARED_FENCE=1 only for an isolated drill"
fi
artifact_root="${ORCHESTRATOR_ARTIFACT_DIR:-}"
[[ -n "$artifact_root" && -d "$artifact_root" ]] || die "ORCHESTRATOR_ARTIFACT_DIR must be an existing directory"
artifact_root="$(cd "$artifact_root" && pwd -P)"
[[ "$artifact_root" != "/" && "$artifact_root" != "${HOME:-}" ]] || \
  die "artifact root must be a dedicated directory"
if [[ -n "${ORCHESTRATOR_HEALTH_URL:-}" ]]; then
  command -v curl >/dev/null 2>&1 || die "curl is required when ORCHESTRATOR_HEALTH_URL is set"
  if curl -fsS --max-time 2 "$ORCHESTRATOR_HEALTH_URL/api/v1/healthz/live" >/dev/null 2>&1; then
    die "daemon is still live; stop it before taking a database + artifact backup"
  fi
fi
backup_root="${ORCHESTRATOR_BACKUP_DIR:-}"
[[ -n "$backup_root" ]] || die "ORCHESTRATOR_BACKUP_DIR is required"
mkdir -p "$backup_root"
backup_root="$(cd "$backup_root" && pwd -P)"
[[ "$backup_root" != "/" && "$backup_root" != "${HOME:-}" ]] || die "backup root must be a dedicated directory"
case "$backup_root/" in
  "$artifact_root/"* ) die "backup root must not be inside the artifact directory" ;;
esac
case "$artifact_root/" in
  "$backup_root/"* ) die "artifact directory must not be inside the backup root" ;;
esac
retention_days="${ORCHESTRATOR_BACKUP_RETENTION_DAYS:-30}"
[[ "$retention_days" =~ ^[0-9]+$ && "$retention_days" -ge 1 ]] || die "retention days must be a positive integer"

stamp="${ORCHESTRATOR_BACKUP_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
final="$backup_root/$stamp"
temporary="$backup_root/.${stamp}.tmp.$$"
[[ ! -e "$final" && ! -e "$temporary" ]] || die "backup target already exists"
umask 077
mkdir -p "$temporary"
cleanup() { local rc=$?; [[ ! -e "$temporary" ]] || rm -rf -- "$temporary"; exit "$rc"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

pg_dump --format=custom --compress=9 --no-owner --no-acl \
  --file "$temporary/orchestrator.dump" "$ORCHESTRATOR_DATABASE_URL"
pg_restore --list "$temporary/orchestrator.dump" >"$temporary/orchestrator.dump.list"
tar -C "$artifact_root" -czf "$temporary/orchestrator-artifacts.tar.gz" .
artifact_files="$(find "$artifact_root" -type f | wc -l | tr -d ' ')"
artifact_bytes="$(find "$artifact_root" -type f -printf '%s\n' | awk '{ total += $1 } END { print total + 0 }')"
cat >"$temporary/manifest.json" <<EOF
{
  "schema_version": 1,
  "product": "OJOS Orchestrator",
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "format": "postgres-custom",
  "database": "orchestrator",
  "artifact_archive": "orchestrator-artifacts.tar.gz",
  "artifact_files": $artifact_files,
  "artifact_bytes": $artifact_bytes,
  "consistency": "control-plane-quiesced",
  "fence_id_sha256": "$(printf '%s' "$ORCHESTRATOR_BACKUP_FENCE_TOKEN" | sha256sum | awk '{print $1}')"
}
EOF
(
  cd "$temporary"
  find . -maxdepth 1 -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS
  sha256sum -c SHA256SUMS >/dev/null
)
mv "$temporary" "$final"
trap - EXIT INT TERM HUP

# Retention only touches timestamp-shaped directories directly below the
# explicitly configured backup root.
find "$backup_root" -mindepth 1 -maxdepth 1 -type d \
  -name '????????T??????Z' -mtime "+$retention_days" -print -exec rm -rf -- {} +
echo "orchestrator-backup: completed $final"
