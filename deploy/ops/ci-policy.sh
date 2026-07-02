#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
bash_bin="${BASH:-bash}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "ops-ci: $1 is required" >&2
    exit 1
  }
}

need_cmd "$bash_bin"
need_cmd docker

shopt -s globstar nullglob
for script in "$script_dir"/**/*.sh; do
  "$bash_bin" -n "$script"
done

if grep -R --line-number \
  --include='*.yaml' \
  --include='*.yml' \
  --include='Dockerfile' \
  -E 'minio/minio:latest|redis:8([[:space:]]|$)' \
  "$repo_root/deploy" "$repo_root/services" >/tmp/ojos-floating-images.log 2>&1; then
  cat /tmp/ojos-floating-images.log >&2
  echo "ops-ci: production runtime images must use fixed tags" >&2
  exit 1
fi

grep -q 'ARG NSJAIL_COMMIT=d6454b4640b6d8699b532a8afa37e4d67e477078' "$repo_root/services/judge-worker/Dockerfile" || {
  echo "ops-ci: judge-worker Dockerfile must pin the reviewed nsjail commit" >&2
  exit 1
}
grep -q 'nsjail_commit: d6454b4640b6d8699b532a8afa37e4d67e477078' "$repo_root/services/judge-worker/config/runtime-lock.yaml" || {
  echo "ops-ci: judge-worker runtime lock must match the Dockerfile nsjail commit" >&2
  exit 1
}
grep -q -- '--seccomp_string' "$repo_root/services/judge-worker/src/sandbox.rs" || {
  echo "ops-ci: judge-worker nsjail runs must install a seccomp policy" >&2
  exit 1
}
if grep -q '"/usr", "/usr"' "$repo_root/services/judge-worker/src/sandbox.rs"; then
  echo "ops-ci: judge-worker sandbox must not bind-mount the entire /usr tree" >&2
  exit 1
fi

if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$repo_root/.env.example" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-weak-secret-check.log 2>&1; then
  echo "ops-ci: weak root .env.example unexpectedly passed secret policy" >&2
  exit 1
fi

strong_env="$(mktemp)"
trap 'rm -f "$strong_env"' EXIT
cat >"$strong_env" <<'EOF'
OJOS_ENVIRONMENT=production
OJOS_PUBLIC_BASE_URL=https://ojos.invalid

AUTH_POSTGRES_DB=ojos_auth
AUTH_POSTGRES_USER=ojos_auth_app
AUTH_POSTGRES_PASSWORD=AuthDbProd_0123456789abcdef
AUTH_DATABASE_URL=postgres://ojos_auth_app:AuthDbProd_0123456789abcdef@auth-db:5432/ojos_auth?sslmode=disable

PROBLEM_POSTGRES_DB=ojos_problem
PROBLEM_POSTGRES_USER=ojos_problem_app
PROBLEM_POSTGRES_PASSWORD=ProblemDbProd_0123456789abcd
PROBLEM_DATABASE_URL=postgres://ojos_problem_app:ProblemDbProd_0123456789abcd@problem-db:5432/ojos_problem?sslmode=disable

JUDGE_POSTGRES_DB=ojos_judge
JUDGE_POSTGRES_USER=ojos_judge_app
JUDGE_POSTGRES_PASSWORD=JudgeDbProd_0123456789abcdef
JUDGE_DATABASE_URL=postgres://ojos_judge_app:JudgeDbProd_0123456789abcdef@judge-db:5432/ojos_judge?sslmode=disable

USER_POSTGRES_DB=ojos_user
USER_POSTGRES_USER=ojos_user_app
USER_POSTGRES_PASSWORD=UserDbProd_0123456789abcdef
USER_DATABASE_URL=postgres://ojos_user_app:UserDbProd_0123456789abcdef@user-db:5432/ojos_user?sslmode=disable

ORCHESTRATOR_POSTGRES_DB=ojos_orchestrator
ORCHESTRATOR_POSTGRES_USER=ojos_orchestrator_app
ORCHESTRATOR_POSTGRES_PASSWORD=OrchestratorDbProd_0123456789
ORCHESTRATOR_DATABASE_URL=postgres://ojos_orchestrator_app:OrchestratorDbProd_0123456789@orchestrator-db:5432/ojos_orchestrator?sslmode=disable

REDIS_PASSWORD=RedisProd_0123456789abcdef012345
REDIS_URL=redis://:RedisProd_0123456789abcdef012345@redis:6379/0
JWT_SECRET=JwtProd_0123456789abcdef0123456789abcdef
AUTH_INTERNAL_TOKEN=AuthIntProd_0123456789abcdef0123456789
ORCHESTRATOR_INTERNAL_TOKEN=OrchIntProd_0123456789abcdef0123456789
OJOS_WORKER_TOKEN=WorkerAuthProd_0123456789abcdef01234567

MINIO_ROOT_USER=prodminioaccess
MINIO_ROOT_PASSWORD=MinioRootProd_0123456789abcdef012345
MINIO_ENDPOINT=minio:9000
MINIO_ACCESS_KEY=prodminioaccess
MINIO_SECRET_KEY=MinioAccessProd_0123456789abcdef0123
MINIO_USE_SSL=false
OJOS_STORAGE_BUCKETS=problems,submissions,judge-artifacts,avatars

OJOS_RUNNER_MODE=nsjail
OJOS_ALLOW_CGROUP_FALLBACK=false
OJOS_NSJAIL_NO_PIVOTROOT=false
OJOS_WORKER_ID=worker-node-01
OJOS_WORKER_NAME=Worker Node 01
OJOS_JUDGE_API_URL=http://judge-api:8082
OJOS_MAX_CONCURRENCY=1
OJOS_SUPPORTED_LANGUAGES=cpp17,c11,python3,java17
OJOS_HEARTBEAT_INTERVAL=10
OJOS_TASK_LEASE_TTL=60
OJOS_LOG_LEVEL=info

OJOS_BACKUP_DIR=/var/backups/ojos
OJOS_STORAGE_ROOT=/var/lib/ojos/storage
OJOS_REDIS_RDB_PATH=/var/lib/redis/dump.rdb
OJOS_ALERT_WEBHOOK_URL=https://alerts.invalid/ojos
GRAFANA_ADMIN_PASSWORD=GrafanaAdminProd_0123456789abcdef01
EOF

OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$strong_env" "$bash_bin" "$script_dir/secret-check.sh"

rendered="$(mktemp)"
trap 'rm -f "$strong_env" "$rendered"' EXIT
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config >"$rendered"
grep -q 'OJOS_RUNNER_MODE: nsjail' "$rendered" || {
  echo "ops-ci: judge-worker must render with OJOS_RUNNER_MODE=nsjail" >&2
  exit 1
}
runner_lines="$(grep 'OJOS_RUNNER_MODE:' "$rendered" || true)"
if [[ -n "$runner_lines" ]] && grep -v 'OJOS_RUNNER_MODE: nsjail' <<<"$runner_lines" >/dev/null; then
  echo "ops-ci: unsupported judge-worker runner mode rendered" >&2
  exit 1
fi
if grep -q 'OJOS_ALLOW_CGROUP_FALLBACK: "true"' "$rendered" || grep -q 'OJOS_ALLOW_CGROUP_FALLBACK: true' "$rendered"; then
  echo "ops-ci: cgroup fallback must not render enabled" >&2
  exit 1
fi
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config --quiet
docker compose --env-file "$strong_env" -f "$repo_root/deploy/worker/docker-compose.yml" config --quiet
docker compose --env-file "$strong_env" -f "$repo_root/deploy/ops/monitoring/docker-compose.yml" config --quiet

echo "ops-ci: production ops policy passed"
