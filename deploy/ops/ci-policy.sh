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
bootstrap_fixture_dir="$(mktemp -d)"
admin_bootstrap_file="$bootstrap_fixture_dir/admin-bootstrap"
observability_token_file="$(mktemp)"
prometheus_observability_token_file="$(mktemp)"
trap 'rm -f "$strong_env" "$admin_bootstrap_file" "$observability_token_file" "$prometheus_observability_token_file"; rmdir "$bootstrap_fixture_dir" 2>/dev/null || true' EXIT
printf '%s\n' 'AdminBootstrapProd_0123456789abcdef0123456789' >"$admin_bootstrap_file"
printf '%s\n' 'ObservabilityProd_0123456789abcdef0123456789' >"$observability_token_file"
printf '%s\n' 'ObservabilityProd_0123456789abcdef0123456789' >"$prometheus_observability_token_file"
chmod 600 "$admin_bootstrap_file" "$observability_token_file" "$prometheus_observability_token_file"
admin_bootstrap_policy_supported=1
platform="$(uname -s)"
case "$platform" in
  MSYS_*|MINGW*|CYGWIN*)
    # Windows-backed Git worktrees cannot represent the production numeric
    # owner contract. Keep the checker itself strict and disable only the
    # optional bootstrap fixture in this local policy runner.
    admin_bootstrap_policy_supported=0
    ;;
esac

set_fixture_owner() {
  local owner="$1"
  shift
  if (( EUID == 0 )); then
    chown "$owner" "$@"
  else
    need_cmd sudo
    sudo -n chown "$owner" "$@"
  fi
}

run_production_secret_check() {
  local env_file="$1"
  if (( admin_bootstrap_policy_supported )) && (( EUID != 0 )); then
    sudo -n env \
      OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 \
      OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 \
      OJOS_ENV_FILE="$env_file" \
      "$bash_bin" "$script_dir/secret-check.sh"
  else
    OJOS_SECRET_CHECK_REQUIRE_ALERTS=1 \
      OJOS_SECRET_CHECK_REQUIRE_MONITORING=1 \
      OJOS_ENV_FILE="$env_file" \
      "$bash_bin" "$script_dir/secret-check.sh"
  fi
}

if (( admin_bootstrap_policy_supported )); then
  set_fixture_owner 65532:65532 "$admin_bootstrap_file"
  [[ "$(stat -c '%u:%g %a' "$admin_bootstrap_file")" == "65532:65532 600" ]] || {
    echo "ops-ci: failed to construct exact 65532:65532/0600 bootstrap fixture" >&2
    exit 1
  }
fi
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
OJOS_BACKUP_TEXTFILE_DIR=/var/lib/ojos/node-exporter-textfile

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
ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN=https://gateway.invalid
ORCHESTRATOR_ALLOW_COMPOSE_BOOTSTRAP_HTTP=1
ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN=http://gateway:8080
ORCHESTRATOR_GATEWAY_ADMIN_TOKEN=GatewayAdminProd_0123456789abcdef0123
ORCHESTRATOR_AUTH_ADMIN_ORIGIN=http://auth-service:8081
ORCHESTRATOR_AUTH_ADMIN_TOKEN=AuthAdminProd_0123456789abcdef012345
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
ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN=GatewayAckProd_0123456789abcdef012345
ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN=AuthAckProd_0123456789abcdef012345678
ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256=sha256:b40b1333ab8013d15668d94ffab2beb0311c9cd73303bebb18bfebce3fbc148a
ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256=sha256:ee5124487f958d8007d69ca541069fbf3dfe5c3da7f41bc8112cd32d35274a55
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
if (( admin_bootstrap_policy_supported )); then
  printf 'AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=%s\n' "$admin_bootstrap_file" >>"$strong_env"
else
  printf 'AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=\n' >>"$strong_env"
fi
printf 'ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE=%s\n' "$observability_token_file" >>"$strong_env"
printf 'PROMETHEUS_ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE=%s\n' "$prometheus_observability_token_file" >>"$strong_env"

run_production_secret_check "$strong_env"

reused_service_token_env="$(mktemp)"
reused_ack_token_env="$(mktemp)"
wrong_ack_verifier_env="$(mktemp)"
wrong_migration_database_env="$(mktemp)"
missing_postgres_ca_env="$(mktemp)"
plaintext_health_env="$(mktemp)"
invalid_bootstrap_file="$bootstrap_fixture_dir/invalid-bootstrap"
public_bootstrap_file="$bootstrap_fixture_dir/public-bootstrap"
wrong_owner_bootstrap_file="$bootstrap_fixture_dir/wrong-owner-bootstrap"
wrong_mode_bootstrap_file="$bootstrap_fixture_dir/wrong-mode-bootstrap"
reused_bootstrap_file="$bootstrap_fixture_dir/reused-bootstrap"
empty_bootstrap_file="$bootstrap_fixture_dir/empty-bootstrap"
oversized_bootstrap_file="$bootstrap_fixture_dir/oversized-bootstrap"
bootstrap_symlink="$bootstrap_fixture_dir/bootstrap-symlink"
printf '%s\n' 'not+url+safe+but-long-enough-token-value' >"$invalid_bootstrap_file"
printf '%s\n' 'AdminBootstrapPublic_0123456789abcdef012345' >"$public_bootstrap_file"
printf '%s\n' 'AdminBootstrapWrongOwner_0123456789abcdef' >"$wrong_owner_bootstrap_file"
printf '%s\n' 'AdminBootstrapWrongMode_0123456789abcdef0' >"$wrong_mode_bootstrap_file"
printf '%s\n' 'JwtProd_0123456789abcdef0123456789abcdef' >"$reused_bootstrap_file"
: >"$empty_bootstrap_file"
printf '%0513d' 0 | tr '0' A >"$oversized_bootstrap_file"
ln -s "$admin_bootstrap_file" "$bootstrap_symlink"
chmod 600 "$invalid_bootstrap_file" "$wrong_owner_bootstrap_file" "$wrong_mode_bootstrap_file" "$reused_bootstrap_file" "$empty_bootstrap_file" "$oversized_bootstrap_file"
chmod 644 "$public_bootstrap_file"
chmod 400 "$wrong_mode_bootstrap_file"
if (( admin_bootstrap_policy_supported )); then
  set_fixture_owner 65532:65532 \
    "$invalid_bootstrap_file" \
    "$public_bootstrap_file" \
    "$wrong_mode_bootstrap_file" \
    "$reused_bootstrap_file" \
    "$empty_bootstrap_file" \
    "$oversized_bootstrap_file"
  set_fixture_owner 0:0 "$wrong_owner_bootstrap_file"
fi
trap 'rm -f "$strong_env" "$admin_bootstrap_file" "$observability_token_file" "$prometheus_observability_token_file" "$reused_service_token_env" "$reused_ack_token_env" "$wrong_ack_verifier_env" "$wrong_migration_database_env" "$missing_postgres_ca_env" "$plaintext_health_env" "$invalid_bootstrap_file" "$public_bootstrap_file" "$wrong_owner_bootstrap_file" "$wrong_mode_bootstrap_file" "$reused_bootstrap_file" "$empty_bootstrap_file" "$oversized_bootstrap_file" "$bootstrap_symlink"; rmdir "$bootstrap_fixture_dir" 2>/dev/null || true' EXIT
sed 's/^OJOS_PROBLEM_SERVICE_TOKEN=.*/OJOS_PROBLEM_SERVICE_TOKEN=UserSvcProd_0123456789abcdef0123456789/' \
  "$strong_env" >"$reused_service_token_env"
if run_production_secret_check "$reused_service_token_env" >/tmp/ojos-reused-service-token.log 2>&1; then
  echo "ops-ci: secret policy unexpectedly accepted a reused service token" >&2
  exit 1
fi
sed 's/^ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN=.*/ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN=GatewayAckProd_0123456789abcdef012345/' \
  "$strong_env" >"$reused_ack_token_env"
if run_production_secret_check "$reused_ack_token_env" >/tmp/ojos-reused-ack-token.log 2>&1; then
  echo "ops-ci: secret policy unexpectedly accepted one ACK token for Gateway and Auth" >&2
  exit 1
fi
sed 's/^ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256=.*/ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' \
  "$strong_env" >"$wrong_ack_verifier_env"
if run_production_secret_check "$wrong_ack_verifier_env" >/tmp/ojos-wrong-ack-verifier.log 2>&1; then
  echo "ops-ci: secret policy unexpectedly accepted an ACK verifier that does not match its raw token" >&2
  exit 1
fi
sed '/^ORCHESTRATOR_MIGRATION_DATABASE_URL=/ s#/ojos_orchestrator?#/wrong_database?#' \
  "$strong_env" >"$wrong_migration_database_env"
if run_production_secret_check "$wrong_migration_database_env" >/tmp/ojos-wrong-migration-database.log 2>&1; then
  echo "ops-ci: migration URL unexpectedly accepted a non-Orchestrator database" >&2
  exit 1
fi
grep -v '^ORCHESTRATOR_POSTGRES_CA_CERT=' "$strong_env" >"$missing_postgres_ca_env"
if run_production_secret_check "$missing_postgres_ca_env" >/tmp/ojos-missing-postgres-ca.log 2>&1; then
  echo "ops-ci: production policy unexpectedly accepted a missing PostgreSQL CA" >&2
  exit 1
fi
sed 's#^ORCHESTRATOR_HEALTHCHECK_URL=https://#ORCHESTRATOR_HEALTHCHECK_URL=http://#' \
  "$strong_env" >"$plaintext_health_env"
if run_production_secret_check "$plaintext_health_env" >/tmp/ojos-plaintext-health.log 2>&1; then
  echo "ops-ci: production policy unexpectedly accepted a plaintext healthcheck" >&2
  exit 1
fi
bootstrap_cases=()
if (( admin_bootstrap_policy_supported )); then
  bootstrap_cases=(invalid public owner mode reused empty oversized symlink)
fi
for bootstrap_case in "${bootstrap_cases[@]}"; do
  case "$bootstrap_case" in
    invalid) bootstrap_path="$invalid_bootstrap_file" ;;
    public) bootstrap_path="$public_bootstrap_file" ;;
    owner) bootstrap_path="$wrong_owner_bootstrap_file" ;;
    mode) bootstrap_path="$wrong_mode_bootstrap_file" ;;
    reused) bootstrap_path="$reused_bootstrap_file" ;;
    empty) bootstrap_path="$empty_bootstrap_file" ;;
    oversized) bootstrap_path="$oversized_bootstrap_file" ;;
    symlink) bootstrap_path="$bootstrap_symlink" ;;
  esac
  bootstrap_env="$(mktemp)"
  sed "s#^AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=.*#AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=$bootstrap_path#" \
    "$strong_env" >"$bootstrap_env"
  if run_production_secret_check "$bootstrap_env" >/tmp/ojos-bootstrap-secret.log 2>&1; then
    rm -f "$bootstrap_env"
    echo "ops-ci: production policy unexpectedly accepted $bootstrap_case admin bootstrap secret file" >&2
    exit 1
  fi
  rm -f "$bootstrap_env"
done
inline_bootstrap_env="$(mktemp)"
printf '%s\n' 'AUTH_ADMIN_BOOTSTRAP_SECRET=InlineBootstrapProd_0123456789abcdef' >>"$strong_env"
if run_production_secret_check "$strong_env" >/tmp/ojos-bootstrap-secret.log 2>&1; then
  echo "ops-ci: production policy unexpectedly accepted inline admin bootstrap secret" >&2
  exit 1
fi
sed '$d' "$strong_env" >"$inline_bootstrap_env"
mv "$inline_bootstrap_env" "$strong_env"

# Compose still needs a concrete host source while its mount contract is being
# rendered. On MSYS/noacl the strict checker above deliberately exercised the
# disabled-bootstrap state; restore the fixture path only for the read-only
# Compose structure assertions below (Docker does not consume the file here).
if (( ! admin_bootstrap_policy_supported )); then
  bootstrap_render_env="$(mktemp)"
  sed "s#^AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=.*#AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=$admin_bootstrap_file#" \
    "$strong_env" >"$bootstrap_render_env"
  mv "$bootstrap_render_env" "$strong_env"
fi

rendered="$(mktemp)"
rendered_json="$(mktemp)"
legacy_rendered="$(mktemp)"
legacy_rendered_json="$(mktemp)"
dev_rendered="$(mktemp)"
trap 'rm -f "$strong_env" "$admin_bootstrap_file" "$observability_token_file" "$prometheus_observability_token_file" "$reused_service_token_env" "$reused_ack_token_env" "$wrong_ack_verifier_env" "$wrong_migration_database_env" "$missing_postgres_ca_env" "$plaintext_health_env" "$invalid_bootstrap_file" "$public_bootstrap_file" "$wrong_owner_bootstrap_file" "$wrong_mode_bootstrap_file" "$reused_bootstrap_file" "$empty_bootstrap_file" "$oversized_bootstrap_file" "$bootstrap_symlink" "$rendered" "$rendered_json" "$legacy_rendered" "$legacy_rendered_json" "$dev_rendered"; rmdir "$bootstrap_fixture_dir" 2>/dev/null || true' EXIT
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config >"$rendered"
docker compose --env-file "$strong_env" -f "$repo_root/deploy/compose/docker-compose.yml" config --format json >"$rendered_json"
if grep -Eq '^[[:space:]]+judge-worker:' "$rendered" || grep -q 'OJOS_RUNNER_MODE:' "$rendered"; then
  echo "ops-ci: production Compose must not render the legacy development Judge Worker" >&2
  exit 1
fi
docker compose --profile legacy-development --env-file "$repo_root/.env.example" \
  -f "$repo_root/deploy/compose/docker-compose.yml" \
  -f "$repo_root/deploy/compose/docker-compose.dev.yml" config >"$legacy_rendered"
docker compose --profile legacy-development --env-file "$repo_root/.env.example" \
  -f "$repo_root/deploy/compose/docker-compose.yml" \
  -f "$repo_root/deploy/compose/docker-compose.dev.yml" config --format json >"$legacy_rendered_json"
grep -Eq '^[[:space:]]+judge-worker:' "$legacy_rendered" || {
  echo "ops-ci: the compatibility Judge Worker must require the explicit legacy-development profile" >&2
  exit 1
}
grep -q 'OJOS_RUNNER_MODE: nsjail' "$legacy_rendered" || {
  echo "ops-ci: the compatibility Judge Worker must retain the nsjail runner" >&2
  exit 1
}
python3 - "$rendered_json" "$repo_root/deploy/compose/docker-compose.yml" <<'PY'
import json
import pathlib
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    services = json.load(stream)["services"]
compose_source = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8")

visiting: list[str] = []
visited: set[str] = set()


def assert_acyclic(service_name: str) -> None:
    if service_name in visited:
        return
    if service_name in visiting:
        cycle_start = visiting.index(service_name)
        cycle = visiting[cycle_start:] + [service_name]
        raise AssertionError(
            "production Compose dependency cycle: " + " -> ".join(cycle)
        )
    visiting.append(service_name)
    for dependency in services[service_name].get("depends_on", {}):
        assert dependency in services, (
            f"{service_name} depends on omitted production service {dependency}"
        )
        assert_acyclic(dependency)
    visiting.pop()
    visited.add(service_name)


for service_name in services:
    assert_acyclic(service_name)

assert "judge-worker" not in services, "legacy Judge Worker rendered in production"
orchestrator = services["orchestrator"]["environment"]
legacy_workloads = {
    "user-service", "storage-service",
    "judge-api", "problem-service",
}
legacy_databases = {"problem-db", "judge-db", "user-db"}
assert legacy_workloads.isdisjoint(services), "legacy business workloads rendered in production"
assert legacy_databases.isdisjoint(services), "legacy per-service databases rendered in production"
for name in ("auth-db", "auth-service-migrations", "auth-service", "gateway"):
    assert name in services, f"platform bootstrap omitted {name}"
assert services["auth-service"]["environment"]["OJOS_PLATFORM_BOOTSTRAP"] == "1"
assert services["auth-service"]["user"] == "65532:65532"
assert services["gateway"]["environment"]["OJOS_PLATFORM_BOOTSTRAP"] == "1"
gateway_environment = services["gateway"]["environment"]
auth_environment = services["auth-service"]["environment"]
assert not any(name in gateway_environment for name in ("OJOS_MANAGED_WORKLOAD", "OJOS_SERVICE_CONTEXT_FILE"))
assert set(services["gateway"]["depends_on"]) == {"auth-service", "orchestrator"}
assert set(services["orchestrator"]["depends_on"]) == {"orchestrator-migrations"}
assert set(services["auth-service"]["networks"]) == {"platform-control"}
assert set(services["gateway"]["networks"]) == {"platform-control"}
assert services["gateway"]["read_only"] is True
assert services["auth-service"]["read_only"] is True

admin_bootstrap_target = "/run/secrets/ojos-auth-admin-bootstrap"
admin_bootstrap_mounts = [
    mount for mount in services["auth-service"].get("volumes", [])
    if mount.get("target") == admin_bootstrap_target
]
assert services["auth-service"]["environment"]["AUTH_ADMIN_BOOTSTRAP_SECRET_FILE"] == admin_bootstrap_target
assert len(admin_bootstrap_mounts) == 1, "platform Auth must receive exactly one admin bootstrap mount"
assert admin_bootstrap_mounts[0].get("type") == "bind", "admin bootstrap secret must be a host bind mount"
assert admin_bootstrap_mounts[0].get("read_only") is True, "admin bootstrap secret mount must be read-only"
target_marker = f"target: {admin_bootstrap_target}"
assert compose_source.count(target_marker) == 1, "admin bootstrap target must be unique in Compose source"
source_mount = compose_source[compose_source.index(target_marker) : compose_source.index(target_marker) + 180]
assert "read_only: true" in source_mount and "create_host_path: false" in source_mount, (
    "admin bootstrap Compose source must explicitly use a read-only, fail-closed bind"
)
# Compose 2.38 uses an omitempty JSON tag for this boolean, so an explicit
# source-level false may be absent from `config --format json`. A serialized
# true is always unsafe; source_mount above remains the authoritative evidence
# that an omitted value came from the required explicit false.
normalized_create_host_path = admin_bootstrap_mounts[0].get("bind", {}).get("create_host_path")
assert normalized_create_host_path is None or normalized_create_host_path is False, (
    "admin bootstrap bind must never create a missing host path"
)
for name, service in services.items():
    if name != "auth-service":
        assert not any(
            mount.get("target") == admin_bootstrap_target
            for mount in service.get("volumes", [])
        ), f"{name} must not receive the Auth admin bootstrap secret"

workload_private_key_target = "/run/secrets/ojos-workload-private-key.pem"
for name, service in services.items():
    mounts = [
        mount
        for mount in service.get("volumes", [])
        if mount.get("target") == workload_private_key_target
    ]
    if name == "auth-service":
        assert len(mounts) == 1, "platform Auth must receive exactly one workload signing key mount"
        assert mounts[0].get("type") == "bind", "Auth workload signing key must be a bind mount"
        assert mounts[0].get("read_only") is True, "Auth workload signing key mount must be read-only"
    else:
        assert not mounts, f"{name} must not receive the Auth workload signing key"

assert orchestrator["OJOS_ENVIRONMENT"] == "production"
assert orchestrator["ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"]
assert orchestrator["ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN"] == "http://gateway:8080"
assert orchestrator["ORCHESTRATOR_AUTH_ADMIN_ORIGIN"] == "http://auth-service:8081"
assert orchestrator["ORCHESTRATOR_GATEWAY_ADMIN_TOKEN"] == gateway_environment["ORCHESTRATOR_GATEWAY_ADMIN_TOKEN"]
assert orchestrator["ORCHESTRATOR_AUTH_ADMIN_TOKEN"] == auth_environment["ORCHESTRATOR_AUTH_ADMIN_TOKEN"]
assert len({
    orchestrator["ORCHESTRATOR_INTERNAL_TOKEN"],
    orchestrator["ORCHESTRATOR_GATEWAY_ADMIN_TOKEN"],
    orchestrator["ORCHESTRATOR_AUTH_ADMIN_TOKEN"],
    orchestrator["ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"],
}) == 4, "platform control-plane credentials must be role-separated"
assert orchestrator["ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN"].startswith("https://")
assert orchestrator["ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN"].startswith("https://")
assert orchestrator["ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE"] == "/run/secrets/orchestrator-observability-token"
PY
if grep -Eq 'target:[[:space:]]+http://(problem-service|judge-api|user-service|storage-service)' \
  "$repo_root/services/gateway/etc/gateway.yaml"; then
  echo "ops-ci: Gateway bootstrap YAML must not contain static business routes" >&2
  exit 1
fi
if grep -Eq 'name:[[:space:]]+(problem-service|judge-api|user-service|storage-service)' \
  "$repo_root/services/gateway/etc/gateway.yaml"; then
  echo "ops-ci: Gateway bootstrap YAML must not trust static business services" >&2
  exit 1
fi
python3 - "$legacy_rendered_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    services = json.load(stream)["services"]

for name in ("gateway", "auth-service", "user-service", "storage-service", "judge-api", "problem-service"):
    assert name in services, f"legacy profile omitted {name}"
for name in ("gateway", "auth-service", "judge-api"):
    assert services[name]["environment"]["OJOS_ENVIRONMENT"] == "development"
assert services["judge-api"]["environment"]["OJOS_ALLOW_LEGACY_WORKER_TOKEN"] == "true"
assert services["judge-worker"]["environment"]["OJOS_ENVIRONMENT"] == "development"
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
  ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN \
  ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE \
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
  'ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE: /run/secrets/orchestrator-observability-token' \
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
  /run/secrets/orchestrator-observability-token \
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
awk '
  index($0, "target: /run/secrets/ojos-workload-private-key.pem") {
    seen = 1
    if ((getline <= 0) || $0 !~ /read_only: true/) bad = 1
  }
  END { exit !(seen && !bad) }
' "$legacy_rendered" || {
  echo "ops-ci: the explicit legacy Auth profile must mount its workload signing key read-only" >&2
  exit 1
}
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
python3 - "$legacy_rendered_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    services = json.load(stream)["services"]

gateway = services["gateway"]
gateway_env = gateway["environment"]
assert set(gateway.get("depends_on", {})) == {"auth-service"}, (
    "development Gateway must depend only on Auth; the retired Orchestrator "
    "0.2 control-plane path must not be revived"
)
assert gateway_env["OJOS_ENVIRONMENT"] == "development"
assert gateway_env.get("OJOS_PLATFORM_BOOTSTRAP") is None
for variable in (
    "ORCHESTRATOR_ENDPOINT",
    "ORCHESTRATOR_INTERNAL_TOKEN",
    "ORCHESTRATOR_NODE_ID",
    "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN",
    "CONTRIBUTION_ACK_TOKEN",
    "OJOS_WORKLOAD_PUBLIC_KEY_FILE",
):
    assert gateway_env.get(variable) == "", (
        f"development Gateway must clear {variable}"
    )

auth = services["auth-service"]
auth_env = auth["environment"]
assert auth_env["OJOS_ENVIRONMENT"] == "development"
assert auth_env.get("OJOS_PLATFORM_BOOTSTRAP") is None
for variable in (
    "AUTH_ADMIN_BOOTSTRAP_SECRET_FILE",
    "ORCHESTRATOR_AUTH_ADMIN_TOKEN",
    "ORCHESTRATOR_ENDPOINT",
    "ORCHESTRATOR_INTERNAL_TOKEN",
    "CONTRIBUTION_ACK_TOKEN",
    "OJOS_WORKLOAD_PRIVATE_KEY_FILE",
    "OJOS_WORKLOAD_CONTROL_PLANE_TOKEN",
):
    assert auth_env.get(variable) == "", (
        f"development Auth must clear {variable}"
    )
assert not any(
    mount.get("target") == "/run/secrets/ojos-auth-admin-bootstrap"
    for mount in auth.get("volumes", [])
), "development Auth must not inherit the production admin bootstrap mount"

for service_name in ("problem-service", "judge-api"):
    service_env = services[service_name]["environment"]
    assert service_env["OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT"] == "http://gateway:8080", (
        f"development {service_name} must use the smoke-pushed delegated permission route"
    )
PY
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
