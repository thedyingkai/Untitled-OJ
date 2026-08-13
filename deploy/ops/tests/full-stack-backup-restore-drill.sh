#!/usr/bin/env bash
set -Eeuo pipefail

die() { echo "full-stack-backup-restore-drill: $*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }
require_env() {
  local name="$1" value="${!1:-}"
  [[ -n "$value" ]] || die "$name is required"
  printf '%s' "$value"
}

for command_name in date env grep id jq pg_dump pg_restore psql python3 redis-check-rdb redis-cli redis-server sha256sum tar tr wc; do
  need_cmd "$command_name"
done
[[ "${OJOS_CONFIRM_FULL_STACK_RESTORE_DRILL:-}" == "full-stack-clean-target-drill-v1" ]] || \
  die "set OJOS_CONFIRM_FULL_STACK_RESTORE_DRILL=full-stack-clean-target-drill-v1"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
evidence_root="$(require_env OJOS_FULL_STACK_DRILL_EVIDENCE_DIR)"
[[ "$evidence_root" == /* && "$evidence_root" != "/" && "$evidence_root" != "${HOME:-}" ]] || \
  die "OJOS_FULL_STACK_DRILL_EVIDENCE_DIR must be an absolute dedicated directory"
mkdir -p "$evidence_root"
evidence_root="$(cd "$evidence_root" && pwd -P)"
[[ "$evidence_root" != "/" && "$evidence_root" != "${HOME:-}" ]] || \
  die "resolved evidence root must be a dedicated directory"
run_id="${OJOS_FULL_STACK_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
[[ "$run_id" =~ ^[0-9]{8}T[0-9]{6}Z(-[A-Za-z0-9._-]+)?$ ]] || die "invalid drill run ID"
run_root="$evidence_root/$run_id"
target_root="$evidence_root/${run_id}-clean-target"
[[ ! -e "$run_root" && ! -e "$target_root" ]] || die "drill run or clean-target directory already exists"
umask 077
mkdir -m 0700 "$run_root"
mkdir -m 0700 "$run_root/work" "$run_root/restore-evidence" "$run_root/target-redis"
mkdir -m 0700 "$target_root"
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

declare -a db_specs=(
  "orchestrator:ORCHESTRATOR_DATABASE_URL:orchestrator_schema_migrations"
  "auth:AUTH_DATABASE_URL:users"
  "problem:PROBLEM_DATABASE_URL:problems"
  "judge:JUDGE_DATABASE_URL:submissions"
  "user:USER_DATABASE_URL:user_profiles"
)
declare -A source_urls=()
declare -A target_urls=()
for spec in "${db_specs[@]}"; do
  name="${spec%%:*}"
  remainder="${spec#*:}"
  source_var="OJOS_DRILL_SOURCE_${name^^}_DATABASE_URL"
  target_var="OJOS_DRILL_TARGET_${name^^}_DATABASE_URL"
  source_urls["$name"]="$(require_env "$source_var")"
  target_urls["$name"]="$(require_env "$target_var")"
  expected_source="ojos_${name}_backup_restore_drill_source"
  expected_target="ojos_${name}_backup_restore_drill_target"
  [[ "$(psql "${source_urls[$name]}" -v ON_ERROR_STOP=1 -Atc 'SELECT current_database()')" == "$expected_source" ]] || \
    die "$source_var must target the dedicated $expected_source database"
  [[ "$(psql "${target_urls[$name]}" -v ON_ERROR_STOP=1 -Atc 'SELECT current_database()')" == "$expected_target" ]] || \
    die "$target_var must target the dedicated $expected_target database"
done

probe_value="full-stack-$run_id"
source_storage="$run_root/source-storage"
target_storage="$target_root/storage"
source_problem_retained="$run_root/source-problem-retained"
target_problem_retained="$target_root/problem-retained"
target_redis_dir="$target_root/redis"
mkdir -m 0700 "$source_storage" "$source_storage/problems/drill" "$source_storage/submissions/drill"
mkdir -m 0700 "$source_problem_retained" "$source_problem_retained/problem-4242"
mkdir -m 0700 "$source_problem_retained/.ojos-mutations"
mkdir -m 0700 "$target_problem_retained" "$target_redis_dir"
printf '{"submissionId":9001,"status":"ACCEPTED","probe":"%s"}\n' "$probe_value" \
  >"$source_storage/submissions/drill/result.json"
submission_artifact_sha256="$(sha256sum "$source_storage/submissions/drill/result.json" | awk '{print $1}')"
printf 'format: ojos\nproblem_no: DRILL-4242\ntitle: %s\n' "$probe_value" \
  >"$source_problem_retained/problem-4242/problem.yaml"
printf 'statement for %s\n' "$probe_value" \
  >"$source_problem_retained/problem-4242/statement.md"
problem_manifest_sha256="$(sha256sum "$source_problem_retained/problem-4242/problem.yaml" | awk '{print $1}')"
python3 - "$source_problem_retained/problem-4242" "$source_storage/problems/drill/package.zip" <<'PY'
import pathlib
import stat
import sys
import zipfile

root = pathlib.Path(sys.argv[1])
output = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        metadata = path.lstat()
        if path.is_symlink() or (not path.is_dir() and not stat.S_ISREG(metadata.st_mode)):
            raise SystemExit(f"unsafe live-tree entry: {path}")
        if path.is_dir():
            continue
        relative = path.relative_to(root).as_posix()
        info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o100644 << 16
        bundle.writestr(info, path.read_bytes(), compresslevel=9)
PY
problem_artifact_sha256="$(sha256sum "$source_storage/problems/drill/package.zip" | awk '{print $1}')"
problem_artifact_size="$(wc -c <"$source_storage/problems/drill/package.zip" | tr -d '[:space:]')"
jq -n \
  --arg artifact_sha256 "$problem_artifact_sha256" \
  --arg updated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  '{
    schema_version: 1,
    operation: "replace",
    problem_id: 4242,
    live_dir: "/data/ojos/problems/problem-4242",
    staging_dir: "/data/ojos/problems/.problem-4242.mutation-staging",
    backup_dir: "/data/ojos/problems/.problem-4242.mutation-backup",
    expected_aggregate_version: 6,
    target_aggregate_version: 7,
    artifact_sha256: $artifact_sha256,
    phase: "live_published",
    updated_at_utc: $updated_at
  }' >"$source_problem_retained/.ojos-mutations/problem-4242.json"

retained_owner_instance_id="full-stack-problem-instance"
retained_volume_name="$(python3 - "$retained_owner_instance_id" <<'PY'
import hashlib
import sys
owner = sys.argv[1]
digest = hashlib.sha256(f"{owner}\0problem-service\0problem-packages".encode()).hexdigest()
print("ojos-retain-" + digest[:32])
PY
)"
write_retained_inspect() {
  local mountpoint="$1" output="$2"
  jq -n \
    --arg name "$retained_volume_name" \
    --arg mountpoint "$mountpoint" \
    --arg owner "$retained_owner_instance_id" \
    '{
      Name: $name,
      Driver: "local",
      Mountpoint: $mountpoint,
      Scope: "local",
      Options: {},
      Labels: {
        "ojos.managed_by": "orchestrator-agent",
        "ojos.service_id": "problem-service",
        "ojos.runtime_profile_sha256": "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f",
        "ojos.volume_logical_name": "problem-packages",
        "ojos.volume_lifecycle": "retain",
        "ojos.owner_instance_id": $owner,
        "ojos.volume_target": "/data/ojos/problems"
      }
    } | [.]' >"$output"
}
source_retained_inspect="$run_root/source-problem-retained.inspect.json"
target_retained_inspect="$run_root/target-problem-retained.inspect.json"
write_retained_inspect "$source_problem_retained" "$source_retained_inspect"
write_retained_inspect "$target_problem_retained" "$target_retained_inspect"

for spec in "${db_specs[@]}"; do
  name="${spec%%:*}"
  psql "${source_urls[$name]}" -v ON_ERROR_STOP=1 \
    --set=probe_value="$probe_value" <<'SQL'
CREATE TABLE IF NOT EXISTS ojos_full_stack_restore_probe (
  probe_key text PRIMARY KEY,
  probe_value text NOT NULL
);
INSERT INTO ojos_full_stack_restore_probe(probe_key, probe_value)
VALUES ('clean-target', :'probe_value')
ON CONFLICT (probe_key) DO UPDATE SET probe_value = EXCLUDED.probe_value;
SQL
done

# Seed a cross-component domain graph instead of relying on a generic sentinel
# alone. The clean target must restore this graph and its content-addressed
# files consistently before it can be considered eligible for cutover.
psql "${source_urls[orchestrator]}" -v ON_ERROR_STOP=1 \
  --set=probe_value="$probe_value" <<'SQL'
CREATE TABLE orchestrator_schema_migrations (version bigint PRIMARY KEY);
INSERT INTO orchestrator_schema_migrations VALUES (13);
CREATE TABLE orchestrator_api_bindings (
  binding_id text PRIMARY KEY,
  consumer_deployment_id text NOT NULL,
  provider_deployment_id text NOT NULL,
  api_id text NOT NULL,
  binding_state text NOT NULL,
  payload jsonb NOT NULL
);
INSERT INTO orchestrator_api_bindings VALUES (
  'drill-problem-storage-get', 'problem-drill', 'storage-drill',
  'storage.object.get', 'ACTIVE',
  jsonb_build_object(
    'binding_id', 'drill-problem-storage-get',
    'consumer_deployment_id', 'problem-drill',
    'provider_deployment_id', 'storage-drill',
    'api_id', 'storage.object.get',
    'desired_state', 'ACTIVE', 'observed_state', 'ACTIVE',
    'health', 'HEALTHY', 'state', 'ACTIVE', 'probe', :'probe_value'
  )
);
SQL

psql "${source_urls[auth]}" -v ON_ERROR_STOP=1 <<'SQL'
CREATE TABLE users (id bigint PRIMARY KEY, username text NOT NULL UNIQUE);
INSERT INTO users VALUES (7001, 'restore-drill-user');
SQL

psql "${source_urls[user]}" -v ON_ERROR_STOP=1 <<'SQL'
CREATE TABLE user_profiles (user_id bigint PRIMARY KEY, display_name text NOT NULL);
INSERT INTO user_profiles VALUES (7001, 'Restore Drill User');
SQL

psql "${source_urls[problem]}" -v ON_ERROR_STOP=1 \
  --set=artifact_sha256="$problem_artifact_sha256" \
  --set=artifact_size="$problem_artifact_size" \
  --set=manifest_sha256="$problem_manifest_sha256" \
  --set=probe_value="$probe_value" <<'SQL'
CREATE TABLE problems (
  id bigint PRIMARY KEY,
  aggregate_version bigint NOT NULL,
  package_dir text NOT NULL,
  manifest_path text NOT NULL,
  manifest_sha256 text NOT NULL,
  package_artifact_uri text NOT NULL,
  package_artifact_sha256 text NOT NULL,
  package_artifact_size_bytes bigint NOT NULL
);
CREATE TABLE integration_outbox (
  sequence bigserial PRIMARY KEY,
  event_id text NOT NULL UNIQUE,
  aggregate_id text NOT NULL,
  aggregate_version bigint NOT NULL,
  event_type text NOT NULL,
  payload jsonb NOT NULL,
  published_at timestamptz
);
INSERT INTO problems VALUES (
  4242, 7, '/data/ojos/problems/problem-4242', 'problem.yaml',
  :'manifest_sha256', 'storage://problems/drill/package.zip', :'artifact_sha256',
  :'artifact_size'
);
INSERT INTO integration_outbox(event_id, aggregate_id, aggregate_version, event_type, payload, published_at)
VALUES (
  'restore-drill-problem-v7', '4242', 7, 'problem.package.published.v1',
  jsonb_build_object(
    'problemId', 4242, 'aggregateVersion', 7,
    'artifactUri', 'storage://problems/drill/package.zip',
    'artifactSha256', :'artifact_sha256', 'probe', :'probe_value'
  ), now()
);
SQL

psql "${source_urls[judge]}" -v ON_ERROR_STOP=1 \
  --set=problem_artifact_sha256="$problem_artifact_sha256" \
  --set=result_artifact_sha256="$submission_artifact_sha256" <<'SQL'
CREATE TABLE submissions (
  id bigint PRIMARY KEY,
  problem_id bigint NOT NULL,
  user_id bigint NOT NULL,
  status text NOT NULL,
  result_path text NOT NULL,
  result_sha256 text NOT NULL,
  problem_aggregate_version bigint NOT NULL,
  problem_artifact_uri text NOT NULL,
  problem_artifact_sha256 text NOT NULL
);
CREATE TABLE integration_inbox (
  consumer_name text NOT NULL,
  event_id text NOT NULL,
  event_type text NOT NULL,
  received_at timestamptz NOT NULL,
  processed_at timestamptz,
  PRIMARY KEY(consumer_name, event_id)
);
INSERT INTO integration_inbox VALUES (
  'judge-problem-projection', 'restore-drill-problem-v7',
  'problem.package.published.v1', now(), now()
);
INSERT INTO submissions VALUES (
  9001, 4242, 7001, 'ACCEPTED',
  'storage://submissions/drill/result.json', :'result_artifact_sha256',
  7, 'storage://problems/drill/package.zip', :'problem_artifact_sha256'
);
SQL

redis_source_url="$(require_env OJOS_DRILL_SOURCE_REDIS_URL)"
[[ "$(redis-cli -u "$redis_source_url" --raw GET ojos:environment-id)" == \
   "full-stack-backup-restore-drill-source" ]] || \
  die "source Redis must contain ojos:environment-id=full-stack-backup-restore-drill-source"
redis-cli -u "$redis_source_url" SET ojos:backup-restore-drill:probe "$probe_value" >/dev/null

printf '%s\n' "$probe_value" >"$source_storage/full-stack-probe.txt"

backup_root="$run_root/backups"
mkdir -m 0700 "$backup_root"
backup_started="$(date -u +%s)"
env -i PATH="$PATH" HOME="${HOME:-/tmp}" \
OJOS_ENVIRONMENT=drill \
OJOS_ENV_FILE= \
OJOS_BACKUP_SOURCE_ID=full-stack-backup-restore-drill-source \
OJOS_BACKUP_DIR="$backup_root" \
OJOS_BACKUP_STAMP="$run_id" \
OJOS_CONFIRM_QUIESCED_BACKUP=backup-drill-fenced-v1 \
OJOS_BACKUP_FENCE_TOKEN="isolated-$probe_value" \
OJOS_BACKUP_ALLOW_DECLARED_FENCE=1 \
OJOS_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID="$retained_owner_instance_id" \
OJOS_PROBLEM_RETAINED_VOLUME_NAME="$retained_volume_name" \
OJOS_PROBLEM_RETAINED_VOLUME_ROOT="$source_problem_retained" \
OJOS_BACKUP_RETAINED_VOLUME_INSPECT_FILE="$source_retained_inspect" \
OJOS_BACKUP_ALLOW_INSPECT_FIXTURE=1 \
OJOS_BACKUP_CONFIRM_RETAINED_VOLUME_QUIESCED=retained-volume-drill-quiesced-v1 \
ORCHESTRATOR_DATABASE_URL="${source_urls[orchestrator]}" \
AUTH_DATABASE_URL="${source_urls[auth]}" \
PROBLEM_DATABASE_URL="${source_urls[problem]}" \
JUDGE_DATABASE_URL="${source_urls[judge]}" \
USER_DATABASE_URL="${source_urls[user]}" \
REDIS_URL="$redis_source_url" \
OJOS_STORAGE_ROOT="$source_storage" \
MINIO_ENDPOINT= OJOS_BACKUP_MINIO_ENDPOINT= MINIO_ACCESS_KEY= MINIO_SECRET_KEY= \
  bash "$repo_root/deploy/ops/backup.sh"
backup_finished="$(date -u +%s)"
backup_dir="$backup_root/$run_id"

env -i PATH="$PATH" HOME="${HOME:-/tmp}" \
OJOS_ENVIRONMENT=drill \
OJOS_ENV_FILE= \
OJOS_RESTORE_DIR="$backup_dir" \
OJOS_RESTORE_SOURCE_ID=full-stack-backup-restore-drill-source \
OJOS_RESTORE_VERIFY_ONLY=1 \
  bash "$repo_root/deploy/ops/restore.sh"

restore_started="$(date -u +%s)"
env -i PATH="$PATH" HOME="${HOME:-/tmp}" \
OJOS_ENVIRONMENT=drill \
OJOS_ENV_FILE= \
OJOS_RESTORE_DIR="$backup_dir" \
OJOS_RESTORE_SOURCE_ID=full-stack-backup-restore-drill-source \
OJOS_RESTORE_TARGET_ID=full-stack-backup-restore-drill-target \
OJOS_CONFIRM_RESTORE=restore-drill-clean-target-v1 \
OJOS_CONFIRM_CLEAN_TARGET=clean-target-v1 \
OJOS_RESTORE_FENCE_TOKEN="isolated-$probe_value" \
OJOS_RESTORE_ALLOW_DECLARED_FENCE=1 \
OJOS_RESTORE_WORK_ROOT="$run_root/work" \
OJOS_RESTORE_EVIDENCE_DIR="$run_root/restore-evidence" \
ORCHESTRATOR_DATABASE_URL="${target_urls[orchestrator]}" \
AUTH_DATABASE_URL="${target_urls[auth]}" \
PROBLEM_DATABASE_URL="${target_urls[problem]}" \
JUDGE_DATABASE_URL="${target_urls[judge]}" \
USER_DATABASE_URL="${target_urls[user]}" \
OJOS_REDIS_RDB_PATH="$target_redis_dir/dump.rdb" \
OJOS_RESTORE_REDIS_OWNER="$(id -u):$(id -g)" \
OJOS_STORAGE_ROOT="$target_storage" \
OJOS_RESTORE_STORAGE_OWNER="$(id -u):$(id -g)" \
OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID="$retained_owner_instance_id" \
OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_NAME="$retained_volume_name" \
OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_ROOT="$target_problem_retained" \
OJOS_RESTORE_RETAINED_VOLUME_INSPECT_FILE="$target_retained_inspect" \
OJOS_RESTORE_ALLOW_INSPECT_FIXTURE=1 \
OJOS_RESTORE_RETAINED_VOLUME_TARGET_ID=full-stack-backup-restore-drill-target \
OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER="$(id -u):$(id -g)" \
  bash "$repo_root/deploy/ops/restore.sh"
restore_finished="$(date -u +%s)"

for spec in "${db_specs[@]}"; do
  name="${spec%%:*}"
  actual="$(psql "${target_urls[$name]}" -v ON_ERROR_STOP=1 -Atc \
    "SELECT probe_value FROM ojos_full_stack_restore_probe WHERE probe_key = 'clean-target'")"
  [[ "$actual" == "$probe_value" ]] || die "$name database probe was not restored"
done
grep -Fqx "$probe_value" "$target_storage/full-stack-probe.txt" || die "local storage probe was not restored"

binding_projection="$(psql "${target_urls[orchestrator]}" -v ON_ERROR_STOP=1 -Atc \
  "SELECT concat_ws('|', binding_id, consumer_deployment_id, provider_deployment_id, api_id, binding_state, payload->>'state') FROM orchestrator_api_bindings WHERE binding_id = 'drill-problem-storage-get'")"
[[ "$binding_projection" == "drill-problem-storage-get|problem-drill|storage-drill|storage.object.get|ACTIVE|ACTIVE" ]] || \
  die "restored active API Binding projection is inconsistent"

outbox_projection="$(psql "${target_urls[problem]}" -v ON_ERROR_STOP=1 -Atc \
  "SELECT concat_ws('|', event_id, aggregate_id, aggregate_version::text, payload->>'artifactSha256') FROM integration_outbox WHERE event_id = 'restore-drill-problem-v7' AND published_at IS NOT NULL")"
[[ "$outbox_projection" == "restore-drill-problem-v7|4242|7|$problem_artifact_sha256" ]] || \
  die "restored Problem outbox witness is inconsistent"
inbox_projection="$(psql "${target_urls[judge]}" -v ON_ERROR_STOP=1 -Atc \
  "SELECT concat_ws('|', event_id, event_type, (processed_at IS NOT NULL)::text) FROM integration_inbox WHERE consumer_name = 'judge-problem-projection' AND event_id = 'restore-drill-problem-v7'")"
[[ "$inbox_projection" == "restore-drill-problem-v7|problem.package.published.v1|true" ]] || \
  die "restored Judge inbox witness is inconsistent"

restored_manifest_sha256="$(sha256sum "$target_problem_retained/problem-4242/problem.yaml" | awk '{print $1}')"
[[ "$restored_manifest_sha256" == "$problem_manifest_sha256" ]] || \
  die "restored Problem live tree manifest digest changed"
grep -Fqx "statement for $probe_value" "$target_problem_retained/problem-4242/statement.md" || \
  die "restored Problem live tree content is missing"
journal_projection="$(jq -r \
  '[.schema_version,.operation,.problem_id,.expected_aggregate_version,.target_aggregate_version,.artifact_sha256,.phase] | map(tostring) | join("|")' \
  "$target_problem_retained/.ojos-mutations/problem-4242.json")"
[[ "$journal_projection" == "1|replace|4242|6|7|$problem_artifact_sha256|live_published" ]] || \
  die "restored Problem mutation journal is inconsistent"
problem_live_projection="$(psql "${target_urls[problem]}" -v ON_ERROR_STOP=1 -Atc \
  "SELECT concat_ws('|', id, aggregate_version, package_dir, manifest_path, manifest_sha256, package_artifact_sha256) FROM problems WHERE id = 4242")"
[[ "$problem_live_projection" == "4242|7|/data/ojos/problems/problem-4242|problem.yaml|$problem_manifest_sha256|$problem_artifact_sha256" ]] || \
  die "restored Problem database does not reference the retained live tree and immutable artifact"

restored_problem_sha256="$(sha256sum "$target_storage/problems/drill/package.zip" | awk '{print $1}')"
restored_result_sha256="$(sha256sum "$target_storage/submissions/drill/result.json" | awk '{print $1}')"
[[ "$restored_problem_sha256" == "$problem_artifact_sha256" ]] || die "restored Problem package digest changed"
[[ "$restored_result_sha256" == "$submission_artifact_sha256" ]] || die "restored Submission result digest changed"
python3 - "$target_storage/problems/drill/package.zip" "$target_problem_retained/problem-4242" <<'PY'
import pathlib
import stat
import sys
import zipfile

archive = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
expected = {}
for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
    metadata = path.lstat()
    if path.is_symlink() or (not path.is_dir() and not stat.S_ISREG(metadata.st_mode)):
        raise SystemExit(f"unsafe restored live-tree entry: {path}")
    if not path.is_dir():
        expected[path.relative_to(root).as_posix()] = path.read_bytes()
with zipfile.ZipFile(archive) as bundle:
    actual = {}
    for member in bundle.infolist():
        path = pathlib.PurePosixPath(member.filename)
        if member.is_dir() or path.is_absolute() or ".." in path.parts or member.filename in actual:
            raise SystemExit(f"unsafe or duplicate immutable package member: {member.filename}")
        actual[member.filename] = bundle.read(member)
if actual != expected:
    raise SystemExit("restored immutable package does not exactly encode restored live tree")
PY
submission_projection="$(psql "${target_urls[judge]}" -v ON_ERROR_STOP=1 -Atc \
  "SELECT concat_ws('|', id, problem_id, user_id, status, result_sha256, problem_aggregate_version, problem_artifact_sha256) FROM submissions WHERE id = 9001")"
[[ "$submission_projection" == "9001|4242|7001|ACCEPTED|$submission_artifact_sha256|7|$problem_artifact_sha256" ]] || \
  die "restored Submission artifact or database references are inconsistent"
[[ "$(psql "${target_urls[problem]}" -v ON_ERROR_STOP=1 -Atc "SELECT count(*) FROM problems WHERE id = 4242 AND aggregate_version = 7 AND package_artifact_sha256 = '$problem_artifact_sha256'")" == "1" ]] || \
  die "restored Submission references a missing Problem revision"
[[ "$(psql "${target_urls[auth]}" -v ON_ERROR_STOP=1 -Atc "SELECT count(*) FROM users WHERE id = 7001")" == "1" ]] || \
  die "restored Submission references a missing Auth user"
[[ "$(psql "${target_urls[user]}" -v ON_ERROR_STOP=1 -Atc "SELECT count(*) FROM user_profiles WHERE user_id = 7001")" == "1" ]] || \
  die "restored Submission references a missing User profile"

jq -n \
  --arg binding "$binding_projection" \
  --arg outbox "$outbox_projection" \
  --arg inbox "$inbox_projection" \
  --arg submission "$submission_projection" \
  --arg problem_live "$problem_live_projection" \
  --arg journal "$journal_projection" \
  --arg manifest_sha256 "$restored_manifest_sha256" \
  --arg problem_sha256 "$restored_problem_sha256" \
  --arg result_sha256 "$restored_result_sha256" \
  '{
    binding: $binding,
    outbox: $outbox,
    inbox: $inbox,
    submission: $submission,
    problem_retained_volume: {
      live_tree: $problem_live,
      journal: $journal,
      manifest_sha256: $manifest_sha256
    },
    problem_artifact_sha256: $problem_sha256,
    submission_result_sha256: $result_sha256,
    database_references: {problem: true, auth_user: true, user_profile: true}
  }' >"$run_root/domain-reconciliation.json"
redis-check-rdb "$target_redis_dir/dump.rdb" >"$run_root/target-redis-check.txt"

redis_runtime="$run_root/redis-runtime"
mkdir -m 0700 "$redis_runtime"
redis_socket="$redis_runtime/redis.sock"
redis_pidfile="$redis_runtime/redis.pid"
redis-server --daemonize no --port 0 --protected-mode yes --appendonly no --save '' \
  --dir "$target_redis_dir" --dbfilename dump.rdb --unixsocket "$redis_socket" \
  --unixsocketperm 600 --pidfile "$redis_pidfile" --logfile "$redis_runtime/redis.log" &
redis_pid=$!
cleanup_redis() {
  local rc=$?
  trap - EXIT INT TERM HUP
  if kill -0 "$redis_pid" >/dev/null 2>&1; then
    redis-cli -s "$redis_socket" shutdown nosave >/dev/null 2>&1 || kill "$redis_pid" >/dev/null 2>&1 || true
  fi
  wait "$redis_pid" 2>/dev/null || true
  exit "$rc"
}
trap cleanup_redis EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
for _ in {1..100}; do
  [[ -S "$redis_socket" ]] && redis-cli -s "$redis_socket" ping >/dev/null 2>&1 && break
  sleep 0.1
done
[[ "$(redis-cli -s "$redis_socket" --raw GET ojos:backup-restore-drill:probe)" == "$probe_value" ]] || \
  die "Redis probe was not restored"
redis-cli -s "$redis_socket" shutdown nosave >/dev/null
wait "$redis_pid"
trap - EXIT INT TERM HUP

manifest_digest="$(sha256sum "$backup_dir/manifest.json" | awk '{print $1}')"
{
  printf 'run_id=%s\n' "$run_id"
  printf 'manifest_sha256=%s\n' "$manifest_digest"
  printf 'backup_seconds=%s\n' "$((backup_finished - backup_started))"
  printf 'restore_seconds=%s\n' "$((restore_finished - restore_started))"
  printf 'postgres_databases=5/5\n'
  printf 'redis_probe=restored\n'
  printf 'local_storage_probe=restored\n'
  printf 'problem_retained_volume_identity=verified\n'
  printf 'problem_retained_live_tree=restored\n'
  printf 'problem_mutation_journal=reconciled\n'
  printf 'problem_artifact_reference=reconciled\n'
  printf 'api_binding_projection=reconciled\n'
  printf 'outbox_inbox_projection=reconciled\n'
  printf 'submission_artifacts_and_database_references=reconciled\n'
  printf 'traffic_changed=no\n'
} >"$run_root/result.txt"

echo "full-stack-backup-restore-drill: passed; clean target remains isolated; evidence=$run_root"
