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
need_cmd python3

shopt -s globstar nullglob
for script in "$script_dir"/**/*.sh; do
  "$bash_bin" -n "$script"
done

python3 -m unittest discover -s "$script_dir/tests" -p 'test_*.py'

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
ORCHESTRATOR_DATABASE_URL=postgres://ojos_orchestrator_app:OrchestratorDbProd_0123456789@orchestrator-db:5432/ojos_orchestrator?sslmode=require
ORCHESTRATOR_MIGRATION_DATABASE_URL=postgres://ojos_orchestrator_app:OrchestratorDbProd_0123456789@orchestrator-db:5432/ojos_orchestrator?sslmode=verify-full&sslrootcert=/run/secrets/orchestrator-postgres-ca.crt
ORCHESTRATOR_POSTGRES_CA_CERT=/run/secrets/orchestrator-postgres-ca.crt
ORCHESTRATOR_HEALTHCHECK_URL=https://orchestrator:8090/api/v1/healthz/ready
ORCHESTRATOR_HEALTHCHECK_CA_CERT=/run/secrets/orchestrator-health-ca.crt
ORCHESTRATOR_TLS_CERT=/run/secrets/orchestrator-tls.crt
ORCHESTRATOR_TLS_KEY=/run/secrets/orchestrator-tls.key
ORCHESTRATOR_NODE_CA_CERT=/run/secrets/orchestrator-node-ca.crt
ORCHESTRATOR_NODE_CA_KEY=/run/secrets/orchestrator-node-ca.key
ORCHESTRATOR_GATEWAY_WORKLOAD_CA_CERT=/run/secrets/gateway-workload-ca.crt
OJOS_WORKLOAD_PRIVATE_KEY_FILE=/run/secrets/ojos-workload-private-key.pem
OJOS_WORKLOAD_PUBLIC_KEY_FILE=/run/secrets/ojos-workload-public-key.pem
OJOS_WORKLOAD_KEY_ID=workload-1
OJOS_WORKLOAD_ISSUER=ojos-auth/workload
OJOS_WORKLOAD_AUDIENCE=ojos-gateway
ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN=http://auth-service:8081
ORCHESTRATOR_AUTH_WORKLOAD_TOKEN=WorkloadIssuerProd_0123456789abcdef012345
ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN=https://gateway.invalid
ORCHESTRATOR_ARTIFACT_DIR=/var/lib/ojos/orchestrator/artifacts
ORCHESTRATOR_MAX_WORKERS=160
ORCHESTRATOR_LOG_RETENTION_DAYS=30
ORCHESTRATOR_ARTIFACT_RETENTION_DAYS=30
ORCHESTRATOR_ARTIFACT_QUOTA_BYTES=10737418240
ORCHESTRATOR_CATALOG_TRUST_KEYS={"catalog-prod":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}
ORCHESTRATOR_CATALOG_SOURCES=[{"id":"official","url":"https://catalog.invalid/v2/catalog.json","required_key_id":"catalog-prod","enabled":true,"auth_secret_ref":""}]
ORCHESTRATOR_OIDC_ISSUER=https://identity.invalid
ORCHESTRATOR_OIDC_AUDIENCE=ojos-orchestrator
ORCHESTRATOR_OIDC_CLIENT_ID=ojos-orchestrator-web
ORCHESTRATOR_OIDC_SCOPES=openid profile email
ORCHESTRATOR_PUBLIC_BASE_URL=https://orchestrator.invalid
ORCHESTRATOR_OIDC_ROLE_CLAIM=roles
ORCHESTRATOR_OIDC_VIEWER_ROLE=viewer
ORCHESTRATOR_OIDC_OPERATOR_ROLE=operator
ORCHESTRATOR_OIDC_ADMIN_ROLE=admin
ORCHESTRATOR_OIDC_JWKS_CACHE_SECONDS=300
ORCHESTRATOR_OIDC_HTTP_TIMEOUT_SECONDS=5
OJOS_GITHUB_TOKEN=GitHubCatalogProd_0123456789abcdef012345
GITHUB_TOKEN=GitHubFallbackProd_0123456789abcdef0123

REDIS_PASSWORD=RedisProd_0123456789abcdef012345
REDIS_URL=redis://:RedisProd_0123456789abcdef012345@redis:6379/0
JWT_SECRET=JwtProd_0123456789abcdef0123456789abcdef
AUTH_INTERNAL_TOKEN=AuthIntProd_0123456789abcdef0123456789
ORCHESTRATOR_INTERNAL_TOKEN=OrchIntProd_0123456789abcdef0123456789
ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1
OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT=http://gateway:8080
OJOS_AUTH_PERMISSION_CHECK_API_ID=auth.user.permission.check
OJOS_USER_SERVICE_TOKEN=UserSvcProd_0123456789abcdef0123456789
OJOS_PROBLEM_SERVICE_TOKEN=ProblemSvcProd_0123456789abcdef0123456
OJOS_JUDGE_API_SERVICE_TOKEN=JudgeApiSvcProd_0123456789abcdef012345

MINIO_ROOT_USER=prodminioaccess
MINIO_ROOT_PASSWORD=MinioRootProd_0123456789abcdef012345
MINIO_ENDPOINT=minio:9000
MINIO_ACCESS_KEY=prodminioaccess
MINIO_SECRET_KEY=MinioAccessProd_0123456789abcdef0123
MINIO_USE_SSL=false
OJOS_STORAGE_BUCKETS=problems,submissions,judge-artifacts,avatars

OJOS_BACKUP_DIR=/var/backups/ojos
OJOS_STORAGE_ROOT=/var/lib/ojos/storage
OJOS_REDIS_RDB_PATH=/var/lib/redis/dump.rdb
OJOS_ALERT_WEBHOOK_URL=https://alerts.invalid/ojos
GRAFANA_ADMIN_PASSWORD=GrafanaAdminProd_0123456789abcdef01
EOF

OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$strong_env" "$bash_bin" "$script_dir/secret-check.sh"

reused_service_token_env="$(mktemp)"
wrong_migration_database_env="$(mktemp)"
missing_postgres_ca_env="$(mktemp)"
plaintext_health_env="$(mktemp)"
trap 'rm -f "$strong_env" "$reused_service_token_env" "$wrong_migration_database_env" "$missing_postgres_ca_env" "$plaintext_health_env"' EXIT
sed 's/^OJOS_PROBLEM_SERVICE_TOKEN=.*/OJOS_PROBLEM_SERVICE_TOKEN=UserSvcProd_0123456789abcdef0123456789/' \
  "$strong_env" >"$reused_service_token_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$reused_service_token_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-reused-service-token.log 2>&1; then
  echo "ops-ci: secret policy unexpectedly accepted a reused service token" >&2
  exit 1
fi
sed '/^ORCHESTRATOR_MIGRATION_DATABASE_URL=/ s#/ojos_orchestrator?#/wrong_database?#' \
  "$strong_env" >"$wrong_migration_database_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$wrong_migration_database_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-wrong-migration-database.log 2>&1; then
  echo "ops-ci: migration URL unexpectedly accepted a non-Orchestrator database" >&2
  exit 1
fi
grep -v '^ORCHESTRATOR_POSTGRES_CA_CERT=' "$strong_env" >"$missing_postgres_ca_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$missing_postgres_ca_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-missing-postgres-ca.log 2>&1; then
  echo "ops-ci: production policy unexpectedly accepted a missing PostgreSQL CA" >&2
  exit 1
fi
sed 's#^ORCHESTRATOR_HEALTHCHECK_URL=https://#ORCHESTRATOR_HEALTHCHECK_URL=http://#' \
  "$strong_env" >"$plaintext_health_env"
if OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 OJOS_ENV_FILE="$plaintext_health_env" "$bash_bin" "$script_dir/secret-check.sh" >/tmp/ojos-plaintext-health.log 2>&1; then
  echo "ops-ci: production policy unexpectedly accepted a plaintext healthcheck" >&2
  exit 1
fi

rendered="$(mktemp)"
rendered_json="$(mktemp)"
legacy_rendered="$(mktemp)"
dev_rendered="$(mktemp)"
trap 'rm -f "$strong_env" "$reused_service_token_env" "$wrong_migration_database_env" "$missing_postgres_ca_env" "$plaintext_health_env" "$rendered" "$rendered_json" "$legacy_rendered" "$dev_rendered"' EXIT
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config >"$rendered"
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config --format json >"$rendered_json"
if grep -Eq '^[[:space:]]+judge-worker:' "$rendered" || grep -q 'OJOS_RUNNER_MODE:' "$rendered"; then
  echo "ops-ci: production Compose must not render the legacy development Judge Worker" >&2
  exit 1
fi
docker compose --profile legacy-development --env-file "$repo_root/.env.example" \
  -f "$repo_root/deploy/compose/docker-compose.yml" \
  -f "$repo_root/deploy/compose/docker-compose.dev.yml" config >"$legacy_rendered"
grep -Eq '^[[:space:]]+judge-worker:' "$legacy_rendered" || {
  echo "ops-ci: the compatibility Judge Worker must require the explicit legacy-development profile" >&2
  exit 1
}
grep -q 'OJOS_RUNNER_MODE: nsjail' "$legacy_rendered" || {
  echo "ops-ci: the compatibility Judge Worker must retain the nsjail runner" >&2
  exit 1
}
python3 - "$rendered_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    services = json.load(stream)["services"]

assert "judge-worker" not in services, "legacy Judge Worker rendered in production"
orchestrator = services["orchestrator"]["environment"]
auth = services["auth-service"]["environment"]
gateway = services["gateway"]["environment"]
judge = services["judge-api"]["environment"]

assert orchestrator["OJOS_ENVIRONMENT"] == "production"
assert orchestrator["ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"]
assert orchestrator["ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN"].startswith("https://")
assert auth["OJOS_ENVIRONMENT"] == "production"
assert auth["OJOS_WORKLOAD_CONTROL_PLANE_TOKEN"] == orchestrator["ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"]
assert auth["OJOS_WORKLOAD_PRIVATE_KEY_FILE"] == "/run/secrets/ojos-workload-private-key.pem"
assert "OJOS_WORKLOAD_PUBLIC_KEY_FILE" not in auth
assert gateway["OJOS_ENVIRONMENT"] == "production"
assert gateway["OJOS_WORKLOAD_PUBLIC_KEY_FILE"] == "/run/secrets/ojos-workload-public-key.pem"
assert "OJOS_WORKLOAD_PRIVATE_KEY_FILE" not in gateway
assert judge["OJOS_ENVIRONMENT"] == "production"
assert judge["OJOS_ALLOW_LEGACY_WORKER_TOKEN"] == "false"
assert judge["OJOS_WORKER_TOKEN"] == ""
assert judge["OJOS_WORKLOAD_PUBLIC_KEY_FILE"] == "/run/secrets/ojos-workload-public-key.pem"
PY
if grep -Eq 'ORCHESTRATOR_NODE_(DISPATCH|ENDPOINT|TOKEN|EXECUTE_SERVICE_DRIVER|HOST_IP):' "$rendered"; then
  echo "ops-ci: v1 Compose must not publish the removed Node push/bearer transport" >&2
  exit 1
fi
if grep -Eq 'ORCHESTRATOR_NODE_(DISPATCH|ENDPOINT|TOKEN|EXECUTE_SERVICE_DRIVER|HOST_IP)' \
  "$repo_root/deploy/compose/docker-compose.yml" "$repo_root/deploy/ops/production.env.example"; then
  echo "ops-ci: production files still reference the removed Node push/bearer transport" >&2
  exit 1
fi
grep -q 'claim' "$repo_root/platform/schemas/orchestrator/agent-protocol-v1.yaml" || {
  echo "ops-ci: v1 Agent pull protocol must publish the claim route" >&2
  exit 1
}
grep -q '^ORCHESTRATOR_NODE_CA_CERT=' "$repo_root/.env.production.example" || {
  echo "ops-ci: production configuration must expose the v1 Node mTLS CA" >&2
  exit 1
}
grep -q '^ORCHESTRATOR_NODE_CA_KEY=' "$repo_root/.env.production.example" || {
  echo "ops-ci: production configuration must expose the v1 Node mTLS signer" >&2
  exit 1
}
for variable in \
  ORCHESTRATOR_DATABASE_URL \
  ORCHESTRATOR_POSTGRES_CA_CERT \
  ORCHESTRATOR_ARTIFACT_DIR \
  ORCHESTRATOR_HEALTHCHECK_URL \
  ORCHESTRATOR_HEALTHCHECK_CA_CERT \
  ORCHESTRATOR_TLS_CERT \
  ORCHESTRATOR_TLS_KEY \
  ORCHESTRATOR_NODE_CA_CERT \
  ORCHESTRATOR_NODE_CA_KEY \
  ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN \
  ORCHESTRATOR_AUTH_WORKLOAD_TOKEN \
  ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN \
  ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE \
  ORCHESTRATOR_CATALOG_TRUST_KEYS \
  ORCHESTRATOR_CATALOG_SOURCES \
  ORCHESTRATOR_OIDC_ISSUER \
  ORCHESTRATOR_OIDC_AUDIENCE \
  ORCHESTRATOR_OIDC_CLIENT_ID \
  ORCHESTRATOR_OIDC_SCOPES \
  ORCHESTRATOR_PUBLIC_BASE_URL \
  ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN \
  ORCHESTRATOR_GATEWAY_ADMIN_TOKEN \
  ORCHESTRATOR_AUTH_ADMIN_ORIGIN \
  ORCHESTRATOR_AUTH_ADMIN_TOKEN \
  ORCHESTRATOR_MAX_WORKERS \
  ORCHESTRATOR_LOG_RETENTION_DAYS \
  ORCHESTRATOR_ARTIFACT_RETENTION_DAYS \
  ORCHESTRATOR_ARTIFACT_QUOTA_BYTES \
  OJOS_GITHUB_TOKEN \
  GITHUB_TOKEN
do
  grep -Eq "^[[:space:]]+$variable:" "$rendered" || {
    echo "ops-ci: production Compose did not pass $variable to the daemon" >&2
    exit 1
  }
done
for assignment in \
  'ORCHESTRATOR_POSTGRES_CA_CERT: /run/secrets/orchestrator-postgres-ca.crt' \
  'ORCHESTRATOR_HEALTHCHECK_CA_CERT: /run/secrets/orchestrator-health-ca.crt' \
  'ORCHESTRATOR_TLS_CERT: /run/secrets/orchestrator-tls.crt' \
  'ORCHESTRATOR_TLS_KEY: /run/secrets/orchestrator-tls.key' \
  'ORCHESTRATOR_NODE_CA_CERT: /run/secrets/orchestrator-node-ca.crt' \
  'ORCHESTRATOR_NODE_CA_KEY: /run/secrets/orchestrator-node-ca.key' \
  'ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE: /run/secrets/gateway-workload-ca.crt' \
  'ORCHESTRATOR_HEALTHCHECK_URL: https://orchestrator:8090/api/v1/healthz/ready' \
  'ORCHESTRATOR_LEGACY_API_MODE: gone'
do
  grep -Fq "$assignment" "$rendered" || {
    echo "ops-ci: production Compose did not render fixed assignment $assignment" >&2
    exit 1
  }
done
grep -Fq 'sslmode=verify-full&sslrootcert=/run/secrets/orchestrator-postgres-ca.crt' "$rendered" || {
  echo "ops-ci: migrations must verify PostgreSQL with the mounted CA" >&2
  exit 1
}
grep -Fq 'catalog.invalid/v2/catalog.json' "$rendered" || {
  echo "ops-ci: trusted Catalog v2 sources were not passed through Compose" >&2
  exit 1
}
grep -Fq 'OJOS_GITHUB_TOKEN: GitHubCatalogProd_0123456789abcdef012345' "$rendered" || {
  echo "ops-ci: the preferred private GitHub Catalog token was not passed through Compose" >&2
  exit 1
}
grep -Fq -- '--cacert "$${ORCHESTRATOR_HEALTHCHECK_CA_CERT}"' "$repo_root/deploy/compose/docker-compose.yml" || {
  echo "ops-ci: production readiness healthcheck must verify its HTTPS CA" >&2
  exit 1
}
for target in \
  /run/secrets/orchestrator-postgres-ca.crt \
  /run/secrets/orchestrator-health-ca.crt \
  /run/secrets/orchestrator-tls.crt \
  /run/secrets/orchestrator-tls.key \
  /run/secrets/orchestrator-node-ca.crt \
  /run/secrets/orchestrator-node-ca.key \
  /run/secrets/gateway-workload-ca.crt \
  /run/secrets/ojos-workload-private-key.pem \
  /run/secrets/ojos-workload-public-key.pem
do
  awk -v target="$target" '
    index($0, "target: " target) {
      seen = 1
      if ((getline <= 0) || $0 !~ /read_only: true/) bad = 1
    }
    END { exit !(seen && !bad) }
  ' "$rendered" || {
    echo "ops-ci: $target must be rendered as a read-only bind mount" >&2
    exit 1
  }
done
[[ "$(grep -Fc 'target: /run/secrets/orchestrator-postgres-ca.crt' "$rendered")" -ge 2 ]] || {
  echo "ops-ci: PostgreSQL CA must be mounted into both migration and daemon containers" >&2
  exit 1
}
if grep -Fq -e 'dev-secrets/placeholder' -e 'dev-secrets\placeholder' "$rendered"; then
  echo "ops-ci: production Compose unexpectedly rendered a development placeholder mount" >&2
  exit 1
fi
if grep -Eq 'ORCHESTRATOR_(RELEASE_PACKAGE_(LOAD|ROOT)|AUTH_PERMISSION_SYNC|GATEWAY_ROUTE_PUBLISH)' \
  "$rendered" "$repo_root/deploy/compose/docker-compose.yml" "$repo_root/deploy/ops/production.env.example"; then
  echo "ops-ci: production files still expose removed 0.2 registration/publish switches" >&2
  exit 1
fi

docker compose --env-file "$repo_root/.env.example" \
  -f "$repo_root/deploy/compose/docker-compose.yml" \
  -f "$repo_root/deploy/compose/docker-compose.dev.yml" config >"$dev_rendered"
grep -Fq -- '- --ephemeral' "$dev_rendered" || {
  echo "ops-ci: development Compose must opt into the explicit ephemeral daemon" >&2
  exit 1
}
grep -Fq 'ORCHESTRATOR_DATABASE_URL: ""' "$dev_rendered" || {
  echo "ops-ci: development Compose must not connect the daemon to PostgreSQL" >&2
  exit 1
}
grep -Fq 'ORCHESTRATOR_HEALTHCHECK_URL: http://127.0.0.1:8090/api/v1/healthz/live' "$dev_rendered" || {
  echo "ops-ci: development Compose must use the bounded loopback live check" >&2
  exit 1
}
if grep -Eq 'ORCHESTRATOR_CATALOG_(TRUST_KEYS|SOURCES): ""' "$dev_rendered"; then
  echo "ops-ci: development Compose must unset Catalog variables, not export empty JSON" >&2
  exit 1
fi
for variable in ORCHESTRATOR_CATALOG_TRUST_KEYS ORCHESTRATOR_CATALOG_SOURCES; do
  grep -Eq "^[[:space:]]+$variable: null$" "$dev_rendered" || {
    echo "ops-ci: development Compose must leave $variable unset" >&2
    exit 1
  }
done
for variable in \
  ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN \
  ORCHESTRATOR_GATEWAY_ADMIN_TOKEN \
  ORCHESTRATOR_AUTH_ADMIN_ORIGIN \
  ORCHESTRATOR_AUTH_ADMIN_TOKEN
do
  grep -Eq "^[[:space:]]+$variable: null$" "$dev_rendered" || {
    echo "ops-ci: development Compose must leave optional provider variable $variable unset" >&2
    exit 1
  }
done
grep -Fq -e 'dev-secrets/placeholder' -e 'dev-secrets\placeholder' "$dev_rendered" || {
  echo "ops-ci: development Compose must resolve its harmless placeholder mounts" >&2
  exit 1
}
runner_lines="$(grep 'OJOS_RUNNER_MODE:' "$legacy_rendered" || true)"
if [[ -n "$runner_lines" ]] && grep -v 'OJOS_RUNNER_MODE: nsjail' <<<"$runner_lines" >/dev/null; then
  echo "ops-ci: unsupported judge-worker runner mode rendered" >&2
  exit 1
fi
if grep -q 'OJOS_ALLOW_CGROUP_FALLBACK: "true"' "$legacy_rendered" || grep -q 'OJOS_ALLOW_CGROUP_FALLBACK: true' "$legacy_rendered"; then
  echo "ops-ci: cgroup fallback must not render enabled" >&2
  exit 1
fi
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config --quiet
docker compose --env-file "$repo_root/.env.example" \
  -f "$repo_root/deploy/compose/docker-compose.yml" \
  -f "$repo_root/deploy/compose/docker-compose.dev.yml" config --quiet
docker compose --env-file "$strong_env" -f "$repo_root/deploy/worker/docker-compose.yml" config --quiet
docker compose --env-file "$strong_env" -f "$repo_root/deploy/ops/monitoring/docker-compose.yml" config --quiet

echo "ops-ci: production ops policy passed"
