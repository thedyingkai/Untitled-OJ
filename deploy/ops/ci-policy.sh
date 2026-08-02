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
ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1
ORCHESTRATOR_NODE_DISPATCH=1
ORCHESTRATOR_NODE_ENDPOINT=http://node.internal:8091
ORCHESTRATOR_NODE_TOKEN=NodeDispatchProd_0123456789abcdef012345
OJOS_WORKER_TOKEN=WorkerAuthProd_0123456789abcdef01234567
OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT=http://gateway:8080
OJOS_AUTH_PERMISSION_CHECK_API_ID=auth.user.permission.check
OJOS_USER_SERVICE_TOKEN=UserSvcProd_0123456789abcdef0123456789
OJOS_PROBLEM_SERVICE_TOKEN=ProblemSvcProd_0123456789abcdef0123456
OJOS_JUDGE_API_SERVICE_TOKEN=JudgeApiSvcProd_0123456789abcdef012345
OJOS_JUDGE_WORKER_SERVICE_TOKEN=JudgeWorkerSvcProd_0123456789abcdef01

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

missing_node_token_env="$(mktemp)"
missing_node_endpoint_env="$(mktemp)"
driver_without_host_env="$(mktemp)"
reused_node_token_env="$(mktemp)"
reused_service_token_env="$(mktemp)"
trap 'rm -f "$strong_env" "$missing_node_token_env" "$missing_node_endpoint_env" "$driver_without_host_env" "$reused_node_token_env" "$reused_service_token_env"' EXIT
grep -v '^ORCHESTRATOR_NODE_TOKEN=' "$strong_env" >"$missing_node_token_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$missing_node_token_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-missing-node-token.log 2>&1; then
  echo "ops-ci: enabled node dispatch unexpectedly passed without ORCHESTRATOR_NODE_TOKEN" >&2
  exit 1
fi
grep -v '^ORCHESTRATOR_NODE_ENDPOINT=' "$strong_env" >"$missing_node_endpoint_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$missing_node_endpoint_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-missing-node-endpoint.log 2>&1; then
  echo "ops-ci: enabled node dispatch unexpectedly passed without ORCHESTRATOR_NODE_ENDPOINT" >&2
  exit 1
fi
cp "$strong_env" "$driver_without_host_env"
printf '\nORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER=1\n' >>"$driver_without_host_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$driver_without_host_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-missing-node-host.log 2>&1; then
  echo "ops-ci: enabled node driver unexpectedly passed without ORCHESTRATOR_NODE_HOST_IP" >&2
  exit 1
fi
sed 's/^ORCHESTRATOR_NODE_TOKEN=.*/ORCHESTRATOR_NODE_TOKEN=OrchIntProd_0123456789abcdef0123456789/' \
  "$strong_env" >"$reused_node_token_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$reused_node_token_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-reused-node-token.log 2>&1; then
  echo "ops-ci: node dispatch unexpectedly accepted a reused control-plane token" >&2
  exit 1
fi
sed 's/^OJOS_PROBLEM_SERVICE_TOKEN=.*/OJOS_PROBLEM_SERVICE_TOKEN=UserSvcProd_0123456789abcdef0123456789/' \
  "$strong_env" >"$reused_service_token_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$reused_service_token_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-reused-service-token.log 2>&1; then
  echo "ops-ci: secret policy unexpectedly accepted a reused service token" >&2
  exit 1
fi

rendered="$(mktemp)"
trap 'rm -f "$strong_env" "$missing_node_token_env" "$missing_node_endpoint_env" "$driver_without_host_env" "$reused_node_token_env" "$reused_service_token_env" "$rendered"' EXIT
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config >"$rendered"
grep -q 'OJOS_RUNNER_MODE: nsjail' "$rendered" || {
  echo "ops-ci: judge-worker must render with OJOS_RUNNER_MODE=nsjail" >&2
  exit 1
}
grep -Eq "ORCHESTRATOR_NODE_DISPATCH: [\"']?1[\"']?$" "$rendered" || {
  echo "ops-ci: orchestrator node dispatch flag was not passed through Compose" >&2
  exit 1
}
grep -q 'ORCHESTRATOR_NODE_ENDPOINT: http://node.internal:8091' "$rendered" || {
  echo "ops-ci: orchestrator node endpoint was not passed through Compose" >&2
  exit 1
}
grep -q 'ORCHESTRATOR_NODE_TOKEN: NodeDispatchProd_0123456789abcdef012345' "$rendered" || {
  echo "ops-ci: orchestrator node token was not passed through Compose" >&2
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
